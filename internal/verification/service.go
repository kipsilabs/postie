// Package verification provides the durable, lease-based post-upload
// verification service for the process-wide upload architecture (issue 184).
//
// It is the primary post-check path: instead of holding a queue upload slot
// during propagation delay, completed transfers are recorded durably and this
// service verifies them in the background. It streams a file's manifest,
// issues STAT requests, and persists ONLY the articles that fail. Failed
// articles are then re-posted (reusing their exact Message-ID and headers from
// the manifest) or STAT-rechecked with backoff, using database leases so the
// work survives process restarts.
//
// The service is decoupled from the upload/NNTP layers through the Stater and
// Reposter interfaces so it can be unit-tested in isolation and wired into the
// processor later.
package verification

import (
	"context"
	"errors"
	"io"
	"io/fs"
	"log/slog"
	"time"

	"github.com/kipsilabs/postie/internal/manifest"
	"github.com/kipsilabs/postie/internal/transferstore"
)

// Stater reports whether articles exist on the server. Stat returns
// missing=true only when the server positively confirmed the article does not
// exist (e.g. NNTP 430); a non-nil error means the check itself failed
// (timeout, dropped connection) and the article's presence is UNKNOWN — such
// errors must not consume repost/recheck budget. For StatBatch, the returned
// set contains the message-IDs NOT confirmed present (any per-ID error counts
// as missing; the per-failure Stat in handleFailure re-validates before any
// budget is spent).
type Stater interface {
	Stat(ctx context.Context, messageID string) (missing bool, err error)
	StatBatch(ctx context.Context, messageIDs []string) (missing map[string]struct{}, err error)
}

// Reposter re-posts a single article described by its manifest record, reusing
// the record's Message-ID and headers so the NZB stays correct.
type Reposter interface {
	Repost(ctx context.Context, rec manifest.ArticleRecord) error
}

// Cleaner runs post-verification cleanup for a transfer once it is fully
// verified (deletes originals per policy, removes PAR2 and manifests). It is a
// no-op while the transfer is not yet terminal, and retains everything on
// failure.
type Cleaner interface {
	CleanupTransfer(ctx context.Context, transferID string) (bool, error)
}

// Config controls verification timing and concurrency. Zero values fall back to
// conservative defaults via withDefaults.
type Config struct {
	// StatBatchSize is the number of message-IDs checked per StatBatch call
	// during file verification. 0 = 100.
	StatBatchSize int
	// PropagationDelay is how long to wait after a (re)post before checking.
	PropagationDelay time.Duration
	// MaxReposts is the maximum number of times a missing article is re-posted
	// before falling back to STAT-only deferred checking.
	MaxReposts int
	// DeferredBackoff is the base delay between STAT-only rechecks; it grows
	// exponentially up to MaxBackoff.
	DeferredBackoff time.Duration
	MaxBackoff      time.Duration
	// MaxDeferredChecks caps STAT-only rechecks before the article (and its
	// file) is marked permanently failed.
	MaxDeferredChecks int
	// LeaseDuration is how long a claimed batch is owned before it can be
	// reclaimed by another worker after a crash.
	LeaseDuration time.Duration
	// BatchSize is the maximum number of failures claimed per cycle.
	BatchSize int
	// PollInterval is how often the Run loop checks for due files/failures.
	PollInterval time.Duration
}

func (c Config) withDefaults() Config {
	if c.StatBatchSize <= 0 {
		c.StatBatchSize = 100
	}
	if c.PropagationDelay <= 0 {
		c.PropagationDelay = 10 * time.Second
	}
	if c.MaxReposts < 0 {
		c.MaxReposts = 0
	}
	if c.DeferredBackoff <= 0 {
		c.DeferredBackoff = 30 * time.Second
	}
	if c.MaxBackoff <= 0 {
		c.MaxBackoff = 5 * time.Minute
	}
	if c.MaxDeferredChecks <= 0 {
		c.MaxDeferredChecks = 5
	}
	if c.LeaseDuration <= 0 {
		c.LeaseDuration = 5 * time.Minute
	}
	if c.BatchSize <= 0 {
		c.BatchSize = 10000
	}
	if c.PollInterval <= 0 {
		c.PollInterval = 15 * time.Second
	}
	return c
}

// Service runs durable verification against a transfer store.
type Service struct {
	store    *transferstore.Store
	stater   Stater
	reposter Reposter
	cfg      Config
	owner    string
	now      func() time.Time
	cleaner  Cleaner
	busy     func() bool
	// busySkips counts consecutive cycles deferred by the busy-gate; only
	// touched from the Run goroutine.
	busySkips int
}

// SetCleaner installs the post-verification cleaner invoked when a transfer
// becomes fully verified. Optional; nil disables cleanup.
func (s *Service) SetCleaner(c Cleaner) { s.cleaner = c }

// SetBusyCheck installs a predicate consulted before each verification cycle;
// while it returns true the sweep is deferred to the next tick. Used when the
// verify pool is the upload pool, so background STAT sweeps do not steal
// connections from saturated live uploads (issue #168 slowness). Optional; nil
// never defers.
func (s *Service) SetBusyCheck(f func() bool) { s.busy = f }

// Completed-item verification statuses surfaced to the queue UI.
const (
	statusVerified = "verified"
	statusFailed   = "verification_failed"
)

// reconcileEveryTicks is how many poll ticks pass between periodic
// ReconcileStuck repair passes (~5 minutes at the default 15s PollInterval).
const reconcileEveryTicks = 20

// maxBusySkips is the maximum number of consecutive verification cycles the
// busy-gate may defer before a full cycle is forced anyway (~5 minutes at the
// default 15s PollInterval). Without it, a continuously saturated upload pool
// starves verification — and repost recovery of missing articles — forever.
const maxBusySkips = 20

// finalizeTransfer reflects a transfer's terminal verification outcome onto its
// completed item and, on full success, runs cleanup. It is a no-op until every
// file of the transfer is terminal (verified or verification_failed). On any
// failure it marks the item verification_failed and retains all recovery data;
// on full success it marks the item verified and triggers cleanup.
func (s *Service) finalizeTransfer(ctx context.Context, transferID string) {
	files, err := s.store.ListFilesByTransfer(ctx, transferID)
	if err != nil {
		slog.WarnContext(ctx, "verification: list files for finalize failed", "transfer", transferID, "error", err)
		return
	}
	if len(files) == 0 {
		return
	}

	anyFailed := false
	var completedItemID string
	for _, f := range files {
		if f.CompletedItemID != "" {
			completedItemID = f.CompletedItemID
		}
		switch f.VerificationState {
		case transferstore.StateVerified:
		case transferstore.StateVerificationFailed:
			anyFailed = true
		default:
			return // not terminal yet
		}
	}

	// The files can go terminal before the processor has linked the completed
	// item (MarkUploaded makes them verification-due while Post() is still
	// returning). Finalizing now would no-op the status update AND run
	// destructive cleanup for an item the queue still shows as
	// pending_verification. Defer: the periodic ReconcileStuck pass finalizes
	// this transfer once the link exists.
	if completedItemID == "" {
		slog.InfoContext(ctx, "verification: transfer terminal but completed item not linked yet; deferring finalize", "transfer", transferID)
		return
	}

	status := statusVerified
	if anyFailed {
		status = statusFailed
	}
	if err := s.store.SetCompletedItemVerificationStatus(ctx, completedItemID, status); err != nil {
		slog.WarnContext(ctx, "verification: update completed item status failed", "transfer", transferID, "error", err)
	}

	// Retain everything on failure; only clean up a fully verified transfer.
	if anyFailed || s.cleaner == nil {
		return
	}
	if _, err := s.cleaner.CleanupTransfer(ctx, transferID); err != nil {
		slog.WarnContext(ctx, "verification: cleanup failed", "transfer", transferID, "error", err)
	}
}

// New creates a verification service. owner identifies this worker for lease
// ownership (e.g. a hostname+pid string).
func New(store *transferstore.Store, stater Stater, reposter Reposter, cfg Config, owner string) *Service {
	return &Service{
		store:    store,
		stater:   stater,
		reposter: reposter,
		cfg:      cfg.withDefaults(),
		owner:    owner,
		now:      time.Now,
	}
}

// Run drives verification until ctx is cancelled: on each tick it reclaims
// expired leases, runs the first verification check for any due files, then
// processes due verification failures (re-posts/rechecks). It is intended to be
// started once as a background goroutine, owned by the processor.
func (s *Service) Run(ctx context.Context) {
	// Repair pass: finalize completed items stuck in pending_verification that
	// the normal flow can no longer reach (crash orphans, items whose files
	// went terminal before the finalize fix, transfers whose files went
	// terminal before the completed item was linked, and pre-durable legacy
	// upgrades). See issue #168. Runs at startup and then periodically so
	// deferred finalizes are picked up without a restart.
	if err := s.ReconcileStuck(ctx); err != nil {
		slog.WarnContext(ctx, "verification: startup reconciliation failed", "error", err)
	}

	ticker := time.NewTicker(s.cfg.PollInterval)
	defer ticker.Stop()

	ticks := 0
	for {
		s.runOnce(ctx)
		ticks++
		if ticks%reconcileEveryTicks == 0 {
			if err := s.ReconcileStuck(ctx); err != nil {
				slog.WarnContext(ctx, "verification: periodic reconciliation failed", "error", err)
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

// runOnce performs a single verification cycle.
func (s *Service) runOnce(ctx context.Context) {
	now := s.now()
	if _, err := s.store.ReclaimExpiredLeases(ctx, now); err != nil {
		slog.WarnContext(ctx, "verification: reclaim leases failed", "error", err)
	}
	if s.busy != nil && s.busy() {
		// Live uploads are saturating the shared NNTP pool; let them keep the
		// connections and verify on a later tick — but not indefinitely. On a
		// continuously busy server the gate would otherwise starve
		// verification (and repost recovery) forever, so force a full cycle
		// after maxBusySkips consecutive deferrals.
		if s.busySkips < maxBusySkips {
			s.busySkips++
			return
		}
		slog.InfoContext(ctx, "verification: upload pool busy but verification deferred too long; running cycle anyway",
			"consecutive_skips", s.busySkips)
	}
	s.busySkips = 0
	if _, err := s.VerifyDueFiles(ctx, now, s.cfg.BatchSize); err != nil {
		slog.WarnContext(ctx, "verification: verify due files failed", "error", err)
	}
	if _, err := s.ProcessDueFailures(ctx, now); err != nil {
		slog.WarnContext(ctx, "verification: process due failures failed", "error", err)
	}
}

// VerifyDueFiles runs the first verification check for every file whose check is
// due (verification_state=uploaded, next_check_at<=now), up to limit. A failure
// verifying one file is logged and does not abort the batch. Returns the number
// of files verified.
func (s *Service) VerifyDueFiles(ctx context.Context, now time.Time, limit int) (int, error) {
	due, err := s.store.ListDueFiles(ctx, now, limit)
	if err != nil {
		return 0, err
	}
	processed := 0
	for _, tf := range due {
		if err := ctx.Err(); err != nil {
			return processed, err
		}
		if err := s.VerifyFile(ctx, tf); err != nil {
			slog.WarnContext(ctx, "verification: verify file failed",
				"transfer", tf.TransferID, "file", tf.FileID, "error", err)
			continue
		}
		processed++
	}
	return processed, nil
}

// VerifyFile streams a transfer file's manifest, STATs every article with
// bounded concurrency, and persists only the misses. If no article is missing
// the file is marked verified; otherwise it is marked verifying and the misses
// are left for ProcessDueFailures to re-post/recheck.
func (s *Service) VerifyFile(ctx context.Context, tf transferstore.TransferFile) error {
	r, err := manifest.OpenReader(tf.ManifestPath)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			// The manifest is gone (deleted, or the data dir moved). It will
			// never come back, so fail the file terminally instead of retrying
			// forever — otherwise the completed item stays pending_verification
			// across restarts (issue #168).
			return s.failFileTerminally(ctx, tf, "manifest missing: "+err.Error())
		}
		return s.deferFileCheck(ctx, tf, err)
	}
	defer func() { _ = r.Close() }()

	var missing []manifest.ArticleRecord

	// flush STATs a chunk of records in one batched sweep and collects misses.
	flush := func(chunk []manifest.ArticleRecord) error {
		if len(chunk) == 0 {
			return nil
		}
		ids := make([]string, len(chunk))
		for i, rec := range chunk {
			ids[i] = rec.MessageID
		}
		missingIDs, err := s.stater.StatBatch(ctx, ids)
		if err != nil {
			return err
		}
		for _, rec := range chunk {
			if _, ok := missingIDs[rec.MessageID]; ok {
				missing = append(missing, rec)
			}
		}
		return nil
	}

	chunk := make([]manifest.ArticleRecord, 0, s.cfg.StatBatchSize)
	for {
		rec, err := r.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			// Unreadable/corrupt manifest content: back off (and eventually
			// terminalize) rather than retry every cycle.
			return s.deferFileCheck(ctx, tf, err)
		}

		if err := ctx.Err(); err != nil {
			return err
		}

		chunk = append(chunk, rec)
		if len(chunk) >= s.cfg.StatBatchSize {
			if err := flush(chunk); err != nil {
				if ctx.Err() != nil {
					return err
				}
				return s.deferFileCheck(ctx, tf, err)
			}
			chunk = chunk[:0]
		}
	}
	if err := flush(chunk); err != nil {
		if ctx.Err() != nil {
			return err
		}
		return s.deferFileCheck(ctx, tf, err)
	}

	if err := ctx.Err(); err != nil {
		return err
	}

	if len(missing) == 0 {
		if err := s.store.SetVerificationState(ctx, tf.TransferID, tf.FileID, transferstore.StateVerified, nil, ""); err != nil {
			return err
		}
		s.finalizeTransfer(ctx, tf.TransferID)
		return nil
	}

	// Persist misses for durable re-posting/rechecking.
	next := s.now().Add(s.cfg.PropagationDelay)
	for _, rec := range missing {
		if err := s.store.AddFailure(ctx, transferstore.VerificationFailure{
			TransferID:    tf.TransferID,
			FileID:        tf.FileID,
			ArticleIndex:  rec.Index,
			MessageID:     rec.MessageID,
			Groups:        rec.Groups,
			State:         transferstore.FailurePending,
			NextAttemptAt: next,
		}); err != nil {
			return err
		}
	}
	return s.store.SetVerificationState(ctx, tf.TransferID, tf.FileID, transferstore.StateVerifying, &next, "")
}

// failFileTerminally marks a file verification_failed with reason and
// finalizes its transfer so the completed item leaves pending_verification.
func (s *Service) failFileTerminally(ctx context.Context, tf transferstore.TransferFile, reason string) error {
	slog.WarnContext(ctx, "verification: file failed terminally",
		"transfer", tf.TransferID, "file", tf.FileID, "reason", reason)
	if err := s.store.SetVerificationState(ctx, tf.TransferID, tf.FileID, transferstore.StateVerificationFailed, nil, reason); err != nil {
		return err
	}
	s.finalizeTransfer(ctx, tf.TransferID)
	return nil
}

// deferFileCheck reschedules a file's first verification check after a
// transient error, with exponential backoff; once MaxDeferredChecks attempts
// are exhausted the file is failed terminally instead of retrying forever.
func (s *Service) deferFileCheck(ctx context.Context, tf transferstore.TransferFile, cause error) error {
	attempts := tf.CheckAttempts + 1
	if attempts >= s.cfg.MaxDeferredChecks {
		return s.failFileTerminally(ctx, tf,
			"verification check failed "+
				"after repeated attempts: "+cause.Error())
	}

	backoff := s.cfg.DeferredBackoff << min(attempts-1, 16)
	if backoff > s.cfg.MaxBackoff || backoff <= 0 {
		backoff = s.cfg.MaxBackoff
	}
	next := s.now().Add(backoff)
	slog.WarnContext(ctx, "verification: check deferred",
		"transfer", tf.TransferID, "file", tf.FileID,
		"attempt", attempts, "next_check_at", next, "error", cause)
	return s.store.DeferFileCheck(ctx, tf.TransferID, tf.FileID, next, cause.Error())
}

// ProcessDueFailures claims a batch of due verification failures and handles
// each: re-post (while under MaxReposts) or STAT-only recheck with exponential
// backoff. Returns the number of failures processed. Resolving the last failure
// for a file flips that file to verified; exhausting retries flips it to
// verification_failed.
func (s *Service) ProcessDueFailures(ctx context.Context, now time.Time) (int, error) {
	claimed, err := s.store.ClaimDueFailures(ctx, s.owner, s.cfg.LeaseDuration, s.cfg.BatchSize, now)
	if err != nil {
		return 0, err
	}
	if len(claimed) == 0 {
		return 0, nil
	}

	// Pre-resolve with batched STAT sweeps: most claimed failures are
	// propagation lag and are present by now, and one-by-one STATs made a
	// large batch (up to BatchSize=10000) take one serial round-trip per
	// article. Articles the sweep does NOT confirm present (including per-ID
	// errors) fall through to handleFailure, whose per-article confirming Stat
	// still distinguishes "missing" from "check errored" before any
	// repost/recheck budget is spent. On sweep error everything falls through.
	stillUnresolved := s.batchPreResolve(ctx, claimed)

	touchedFiles := make(map[[2]string]bool)
	var unresolved []transferstore.VerificationFailure
	for _, f := range claimed {
		touchedFiles[[2]string{f.TransferID, f.FileID}] = true
		if _, miss := stillUnresolved[f.MessageID]; !miss {
			f.State = transferstore.FailureResolved
			f.LastError = ""
			f.NextAttemptAt = now
			_ = s.store.UpdateFailureAfterCheck(ctx, f)
			continue
		}
		unresolved = append(unresolved, f)
	}

	// Resolve manifest records for failures that still need a re-post, grouped
	// by file so each manifest is streamed at most once per batch.
	records := s.resolveRecords(ctx, unresolved)

	for _, f := range unresolved {
		if err := ctx.Err(); err != nil {
			return 0, err
		}
		s.handleFailure(ctx, f, records, now)
	}

	// Reconcile each touched file's verification state.
	for key := range touchedFiles {
		s.reconcileFileState(ctx, key[0], key[1])
	}

	return len(claimed), nil
}

// batchPreResolve sweeps the claimed failures' message-IDs with StatBatch
// (chunked to StatBatchSize) and returns the set NOT confirmed present. On a
// sweep error the remaining ids are treated as unresolved so the per-failure
// path decides — never resolved, which could silently drop a real miss.
func (s *Service) batchPreResolve(ctx context.Context, claimed []transferstore.VerificationFailure) map[string]struct{} {
	unresolved := make(map[string]struct{}, len(claimed))
	ids := make([]string, 0, len(claimed))
	seen := make(map[string]struct{}, len(claimed))
	for _, f := range claimed {
		if _, dup := seen[f.MessageID]; dup {
			continue
		}
		seen[f.MessageID] = struct{}{}
		ids = append(ids, f.MessageID)
	}

	batch := s.cfg.StatBatchSize
	for start := 0; start < len(ids); start += batch {
		chunk := ids[start:min(start+batch, len(ids))]
		missing, err := s.stater.StatBatch(ctx, chunk)
		if err != nil {
			for _, id := range ids[start:] {
				unresolved[id] = struct{}{}
			}
			return unresolved
		}
		for id := range missing {
			unresolved[id] = struct{}{}
		}
	}
	return unresolved
}

// handleFailure processes a single claimed failure, updating its row. It
// always STATs the article first: a miss recorded during the initial sweep is
// usually propagation lag, not data loss, and a re-post re-uploads the article
// body — checking first turns a would-be transfer-wide re-upload into one
// cheap STAT round. Only articles confirmed missing since the last (re)post
// spend a repost attempt; the rest defer with backoff until terminal.
func (s *Service) handleFailure(ctx context.Context, f transferstore.VerificationFailure, records map[recordKey]manifest.ArticleRecord, now time.Time) {
	missing, statErr := s.stater.Stat(ctx, f.MessageID)
	if statErr != nil {
		// The check itself failed (timeout, dropped connection, provider
		// hiccup): the article may well exist. Reschedule WITHOUT consuming
		// repost/recheck budget, otherwise a flaky provider re-uploads data
		// that is already there and eventually marks the file
		// verification_failed for no reason.
		f.State = transferstore.FailurePending
		f.NextAttemptAt = now.Add(s.cfg.DeferredBackoff)
		f.LastError = "stat check failed: " + statErr.Error()
		_ = s.store.UpdateFailureAfterCheck(ctx, f)
		return
	}
	if !missing {
		f.State = transferstore.FailureResolved
		f.LastError = ""
		f.NextAttemptAt = now
		_ = s.store.UpdateFailureAfterCheck(ctx, f)
		return
	}

	// Confirmed missing: re-post while budget remains and a source exists.
	if f.RepostCount < s.cfg.MaxReposts {
		if rec, ok := records[recordKey{f.TransferID, f.FileID, f.ArticleIndex}]; ok {
			if err := s.reposter.Repost(ctx, rec); err == nil {
				// Re-posted: schedule a recheck after propagation delay.
				f.RepostCount++
				f.State = transferstore.FailurePending
				f.NextAttemptAt = now.Add(s.cfg.PropagationDelay)
				f.LastError = ""
				_ = s.store.UpdateFailureAfterCheck(ctx, f)
				return
			} else if !errors.Is(err, fs.ErrNotExist) {
				// Re-post failed: consume an attempt so persistent errors stay
				// bounded by MaxReposts, keep pending with a short backoff.
				f.RepostCount++
				f.NextAttemptAt = now.Add(s.cfg.PropagationDelay)
				f.LastError = "repost failed: " + err.Error()
				_ = s.store.UpdateFailureAfterCheck(ctx, f)
				return
			} else {
				// Source file gone (e.g. a temp PAR2 already cleaned up) —
				// re-posting can never succeed; fall through to the bounded
				// deferred-recheck path.
				slog.WarnContext(ctx, "verification: repost source missing, falling back to STAT rechecks",
					"messageID", f.MessageID, "error", err)
			}
		}
		// No manifest record (e.g. legacy STAT-only failure) or unusable
		// source: deferred rechecks below.
	}

	s.deferOrFail(ctx, f, now)
}

// deferOrFail schedules the next STAT-only recheck with exponential backoff,
// or marks the failure permanently failed once rechecks are exhausted.
func (s *Service) deferOrFail(ctx context.Context, f transferstore.VerificationFailure, now time.Time) {
	f.DeferredCount++
	if f.DeferredCount >= s.cfg.MaxDeferredChecks {
		f.State = transferstore.FailureFailed
		f.NextAttemptAt = now
		f.LastError = "exhausted deferred rechecks"
		_ = s.store.UpdateFailureAfterCheck(ctx, f)
		return
	}

	backoff := s.cfg.DeferredBackoff << min(f.DeferredCount-1, 16)
	if backoff > s.cfg.MaxBackoff || backoff <= 0 {
		backoff = s.cfg.MaxBackoff
	}
	f.State = transferstore.FailurePending
	f.NextAttemptAt = now.Add(backoff)
	f.LastError = "article still missing"
	_ = s.store.UpdateFailureAfterCheck(ctx, f)
}

// ReconcileStuck finalizes completed items stuck in pending_verification that
// the normal verification flow can no longer reach:
//
//   - items whose linked transfer files are all terminal but were never
//     finalized (pre-fix bug);
//   - items with no linked transfer files at all — crash orphans and legacy
//     (pre-durable) items — which are resolved from their remaining legacy
//     failures, or marked verified when nothing is outstanding.
//
// Items with live (non-terminal) files or still-pending legacy failures are
// left for the regular verification flow. Intended to run once at startup.
func (s *Service) ReconcileStuck(ctx context.Context) error {
	ids, err := s.store.ListPendingVerificationItemIDs(ctx)
	if err != nil {
		return err
	}
	repaired := 0
	for _, id := range ids {
		if err := ctx.Err(); err != nil {
			return err
		}
		files, err := s.store.ListFilesByCompletedItem(ctx, id)
		if err != nil {
			slog.WarnContext(ctx, "verification: reconcile list files failed", "item", id, "error", err)
			continue
		}
		if len(files) > 0 {
			// finalizeTransfer no-ops unless every file is terminal, so live
			// transfers are untouched.
			s.finalizeTransfer(ctx, files[0].TransferID)
			continue
		}
		if s.reconcileLegacyItem(ctx, id) {
			repaired++
		}
	}
	if repaired > 0 {
		slog.InfoContext(ctx, "verification: repaired stuck pending_verification items", "count", repaired)
	}
	return nil
}

// reconcileLegacyItem resolves a completed item that has no linked transfer
// files from its legacy (STAT-only) failures: outstanding work leaves it
// untouched; otherwise it becomes verified, or verification_failed when any
// legacy failure exhausted its retries. Reports whether the item was updated.
func (s *Service) reconcileLegacyItem(ctx context.Context, completedItemID string) bool {
	outstanding, err := s.store.CountFailures(ctx, completedItemID, transferstore.LegacyFileID, transferstore.FailurePending)
	if err != nil {
		slog.WarnContext(ctx, "verification: reconcile count failed", "item", completedItemID, "error", err)
		return false
	}
	if outstanding > 0 {
		return false // legacy checks still being worked
	}

	failed, err := s.store.CountFailures(ctx, completedItemID, transferstore.LegacyFileID, transferstore.FailureFailed)
	if err != nil {
		slog.WarnContext(ctx, "verification: reconcile count failed", "item", completedItemID, "error", err)
		return false
	}
	status := statusVerified
	if failed > 0 {
		status = statusFailed
	}
	if err := s.store.SetCompletedItemVerificationStatus(ctx, completedItemID, status); err != nil {
		slog.WarnContext(ctx, "verification: reconcile update item failed", "item", completedItemID, "error", err)
		return false
	}
	return true
}

// reconcileFileState flips a file to verified when no pending/reposted failures
// remain, or to verification_failed when only permanently-failed ones do.
func (s *Service) reconcileFileState(ctx context.Context, transferID, fileID string) {
	if fileID == transferstore.LegacyFileID {
		// Legacy STAT-only failures have no transfer_files row; resolve the
		// completed item (keyed by transfer_id) directly.
		s.reconcileLegacyItem(ctx, transferID)
		return
	}
	outstanding, err := s.store.CountFailures(ctx, transferID, fileID, transferstore.FailurePending)
	if err != nil {
		slog.WarnContext(ctx, "verification: count pending failed", "error", err)
		return
	}
	if outstanding > 0 {
		return // still working
	}
	failed, err := s.store.CountFailures(ctx, transferID, fileID, transferstore.FailureFailed)
	if err != nil {
		slog.WarnContext(ctx, "verification: count failed failed", "error", err)
		return
	}
	if failed > 0 {
		_ = s.store.SetVerificationState(ctx, transferID, fileID, transferstore.StateVerificationFailed, nil, "verification failed for some articles")
		// The transfer may now be terminal: propagate the failure to the
		// completed item, otherwise it stays pending_verification forever.
		s.finalizeTransfer(ctx, transferID)
		return
	}
	_ = s.store.SetVerificationState(ctx, transferID, fileID, transferstore.StateVerified, nil, "")
	s.finalizeTransfer(ctx, transferID)
}

type recordKey struct {
	transferID string
	fileID     string
	index      int
}

// resolveRecords streams the manifest of each file that has a re-postable
// failure and returns the manifest records keyed by (transfer,file,index).
func (s *Service) resolveRecords(ctx context.Context, failures []transferstore.VerificationFailure) map[recordKey]manifest.ArticleRecord {
	out := make(map[recordKey]manifest.ArticleRecord)

	// Which indices does each file need (only for failures still eligible to
	// re-post)?
	needed := make(map[[2]string]map[int]bool)
	for _, f := range failures {
		if f.RepostCount >= s.cfg.MaxReposts || f.ArticleIndex < 0 {
			continue
		}
		key := [2]string{f.TransferID, f.FileID}
		if needed[key] == nil {
			needed[key] = make(map[int]bool)
		}
		needed[key][f.ArticleIndex] = true
	}

	for key, indices := range needed {
		tf, err := s.store.GetFile(ctx, key[0], key[1])
		if err != nil {
			slog.WarnContext(ctx, "verification: load transfer file failed", "transfer", key[0], "file", key[1], "error", err)
			continue
		}
		r, err := manifest.OpenReader(tf.ManifestPath)
		if err != nil {
			slog.WarnContext(ctx, "verification: open manifest failed", "path", tf.ManifestPath, "error", err)
			continue
		}
		remaining := len(indices)
		for remaining > 0 {
			rec, err := r.Next()
			if errors.Is(err, io.EOF) || err != nil {
				break
			}
			if indices[rec.Index] {
				out[recordKey{key[0], key[1], rec.Index}] = rec
				remaining--
			}
		}
		_ = r.Close()
	}
	return out
}
