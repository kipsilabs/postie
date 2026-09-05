package progress

import (
	"context"
	"fmt"
	"maps"
	"math"
	"os"
	"sort"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/schollz/progressbar/v3"
)

type ProgressType string

const (
	ProgressTypeUploading      ProgressType = "uploading"
	ProgressTypePar2Generation ProgressType = "par2_generation"
	ProgressTypeChecking       ProgressType = "checking"
)

type ProgressState struct {
	Max                  int64
	CurrentNum           int64
	CurrentPercent       float64
	CurrentBytes         float64
	SecondsSince         float64
	SecondsLeft          float64
	KBsPerSecond         float64
	Description          string
	Type                 ProgressType
	IsStarted            bool
	IsWaiting            bool
	WaitSecondsRemaining float64
	IsPaused             bool
}

// EventEmitter is a function type for emitting events to the frontend
type EventEmitter func(eventType string, data any)

type JobProgress interface {
	AddProgress(id uuid.UUID, name string, pType ProgressType, total int64) Progress
	FinishProgress(id uuid.UUID)
	GetProgress(id uuid.UUID) Progress
	GetAllProgress() map[uuid.UUID]Progress
	GetAllProgressState() []ProgressState
	GetJobID() string
	Close()
	SetAllPaused(paused bool)
}

// Progress represents an individual progress indicator
type Progress interface {
	UpdateProgress(processed int64)
	Finish()
	GetState() ProgressState
	GetID() uuid.UUID
	GetName() string
	GetType() ProgressType
	GetCurrent() int64
	GetTotal() int64
	GetPercentage() float64
	IsComplete() bool
	GetStartTime() time.Time
	GetElapsedTime() time.Duration
	SetPaused(paused bool)
	IsPaused() bool
	SetWaitDeadline(deadline time.Time)
}

type jobProgress struct {
	jobID          string
	activeProgress map[uuid.UUID]Progress
	mu             sync.RWMutex
	ctx            context.Context
	cancel         context.CancelFunc
}

func NewProgressJob(jobID string) JobProgress {
	ctx, cancel := context.WithCancel(context.Background())

	return &jobProgress{
		jobID:          jobID,
		activeProgress: make(map[uuid.UUID]Progress),
		ctx:            ctx,
		cancel:         cancel,
	}
}

func (pm *jobProgress) AddProgress(
	id uuid.UUID,
	name string,
	pType ProgressType,
	total int64,
) Progress {
	pm.mu.Lock()
	defer pm.mu.Unlock()

	// Ensure total is never 0 to prevent division by zero errors
	if total <= 0 {
		total = 1
	}

	var OptionShowBytes progressbar.Option
	if pType == ProgressTypeUploading {
		OptionShowBytes = progressbar.OptionShowBytes(true)
	} else {
		OptionShowBytes = progressbar.OptionShowBytes(false)
	}

	progress := &progress{
		id:        id,
		name:      name,
		pType:     pType,
		total:     total,
		startTime: time.Now(),
		clock:     time.Now,
		progress: progressbar.NewOptions64(
			total,
			progressbar.OptionSetDescription(name),
			OptionShowBytes,
			progressbar.OptionSetWidth(15),
			progressbar.OptionThrottle(100*time.Millisecond),
			progressbar.OptionShowCount(),
			progressbar.OptionOnCompletion(func() {
				_, _ = fmt.Fprint(os.Stdout, "\n")
			}),
			progressbar.OptionSpinnerType(14),
			progressbar.OptionFullWidth(),
			progressbar.OptionSetRenderBlankState(true),
			progressbar.OptionSetTheme(progressbar.Theme{
				Saucer:        "=",
				SaucerHead:    ">",
				SaucerPadding: " ",
				BarStart:      "[",
				BarEnd:        "]",
			}),
			progressbar.OptionSetMaxDetailRow(0),
			progressbar.OptionSetPredictTime(true),
		),
	}

	pm.activeProgress[id] = progress
	return progress
}

func (pm *jobProgress) FinishProgress(id uuid.UUID) {
	pm.mu.Lock()
	defer pm.mu.Unlock()

	if progress, exists := pm.activeProgress[id]; exists {
		progress.Finish()
		delete(pm.activeProgress, id)
	}
}

func (pm *jobProgress) GetProgress(id uuid.UUID) Progress {
	pm.mu.RLock()
	defer pm.mu.RUnlock()

	return pm.activeProgress[id]
}

func (pm *jobProgress) GetAllProgress() map[uuid.UUID]Progress {
	pm.mu.RLock()
	defer pm.mu.RUnlock()

	result := make(map[uuid.UUID]Progress)
	maps.Copy(result, pm.activeProgress)
	return result
}

func (pm *jobProgress) GetAllProgressState() []ProgressState {
	pm.mu.RLock()
	defer pm.mu.RUnlock()

	result := make([]ProgressState, 0, len(pm.activeProgress))
	for _, v := range pm.activeProgress {
		result = append(result, v.GetState())
	}

	sort.Slice(result, func(i, j int) bool {
		// Sort by current progress in descending order, then by description in ascending order
		if result[i].CurrentNum != result[j].CurrentNum {
			return result[i].CurrentNum > result[j].CurrentNum
		}

		return result[i].Description < result[j].Description
	})

	return result
}

func (pm *jobProgress) GetJobID() string {
	return pm.jobID
}

func (pm *jobProgress) Close() {
	pm.cancel()
}

func (pm *jobProgress) SetAllPaused(paused bool) {
	pm.mu.Lock()
	defer pm.mu.Unlock()

	for _, progress := range pm.activeProgress {
		if progress.GetType() == ProgressTypePar2Generation {
			// par2 generation can not be paused
			continue
		}

		progress.SetPaused(paused)
	}
}

type progress struct {
	id           uuid.UUID
	name         string
	pType        ProgressType
	total        int64
	startTime    time.Time
	progress     *progressbar.ProgressBar
	paused       bool
	waitDeadline time.Time
	clock        func() time.Time
	bytes        int64
	firstByteAt  time.Time
	pausedAt     time.Time
	pausedTotal  time.Duration
	mu           sync.RWMutex
}

// activeSeconds is the time spent actually transferring: from the first byte
// onwards, minus any paused intervals. Wall-clock time since the bar was
// created includes queue waits and pauses, which made the displayed speed
// meaningless for jobs that sat behind other uploads.
func (p *progress) activeSeconds(now time.Time) float64 {
	if p.firstByteAt.IsZero() {
		return 0
	}
	active := now.Sub(p.firstByteAt) - p.pausedTotal
	if !p.pausedAt.IsZero() {
		active -= now.Sub(p.pausedAt)
	}
	return active.Seconds()
}

func (p *progress) UpdateProgress(processed int64) {
	if p.progress.IsFinished() {
		return
	}

	p.mu.Lock()
	if p.firstByteAt.IsZero() {
		now := p.clock()
		p.firstByteAt = now
		p.pausedTotal = 0
		if p.paused {
			p.pausedAt = now
		}
	}
	p.bytes += processed
	p.mu.Unlock()

	_ = p.progress.Add64(processed)
}

func (p *progress) Finish() {
	_ = p.progress.Finish()
	_ = p.progress.Close()
}

func (p *progress) GetID() uuid.UUID {
	return p.id
}

func (p *progress) GetName() string {
	return p.name
}

func (p *progress) GetType() ProgressType {
	return p.pType
}

func (p *progress) GetState() ProgressState {
	s := p.progress.State()
	p.mu.RLock()
	paused := p.paused
	waitDeadline := p.waitDeadline
	now := p.clock()
	bytes := p.bytes
	activeSecs := p.activeSeconds(now)
	p.mu.RUnlock()

	kbsPerSecond := 0.0
	secondsLeft := 0.0
	if bytes > 0 && activeSecs > 0 {
		kbsPerSecond = float64(bytes) / 1024.0 / activeSecs
		if remaining := p.total - bytes; remaining > 0 {
			secondsLeft = float64(remaining) / (kbsPerSecond * 1024.0)
		}
	}

	secsRemaining := 0.0
	isWaiting := false
	if !waitDeadline.IsZero() {
		remaining := time.Until(waitDeadline).Seconds()
		if remaining > 0 {
			isWaiting = true
			secsRemaining = remaining
		}
	}

	// Sanitize float64 values to prevent NaN in JSON serialization
	currentPercent := s.CurrentPercent
	if math.IsNaN(currentPercent) || math.IsInf(currentPercent, 0) {
		currentPercent = 0.0
	}

	currentBytes := s.CurrentBytes
	if math.IsNaN(currentBytes) || math.IsInf(currentBytes, 0) {
		currentBytes = 0.0
	}

	secondsSince := s.SecondsSince
	if math.IsNaN(secondsSince) || math.IsInf(secondsSince, 0) {
		secondsSince = 0.0
	}

	return ProgressState{
		Max:                  s.Max,
		CurrentNum:           s.CurrentNum,
		CurrentPercent:       currentPercent,
		CurrentBytes:         currentBytes,
		SecondsSince:         secondsSince,
		SecondsLeft:          secondsLeft,
		KBsPerSecond:         kbsPerSecond,
		Description:          s.Description,
		Type:                 p.pType,
		IsStarted:            s.CurrentNum > 0,
		IsWaiting:            isWaiting,
		WaitSecondsRemaining: secsRemaining,
		IsPaused:             paused,
	}
}

func (p *progress) GetCurrent() int64 {
	return p.progress.State().CurrentNum
}

func (p *progress) GetTotal() int64 {
	return p.total
}

func (p *progress) GetPercentage() float64 {
	percentage := p.progress.State().CurrentPercent
	if math.IsNaN(percentage) || math.IsInf(percentage, 0) {
		return 0.0
	}
	return percentage
}

func (p *progress) IsComplete() bool {
	return p.progress.IsFinished()
}

func (p *progress) GetStartTime() time.Time {
	return p.startTime
}

func (p *progress) GetElapsedTime() time.Duration {
	return time.Duration(p.progress.State().SecondsSince) * time.Second
}

func (p *progress) GetLeftTime() time.Duration {
	return time.Duration(p.progress.State().SecondsLeft) * time.Second
}

func (p *progress) SetPaused(paused bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if paused == p.paused {
		return
	}
	p.paused = paused
	now := p.clock()
	if paused {
		p.pausedAt = now
		return
	}
	p.pausedTotal += now.Sub(p.pausedAt)
	p.pausedAt = time.Time{}
}

func (p *progress) IsPaused() bool {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return p.paused
}

func (p *progress) SetWaitDeadline(deadline time.Time) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.waitDeadline = deadline
}
