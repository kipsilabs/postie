package backend

import (
	"context"
	"fmt"
	"log/slog"

	"github.com/kipsilabs/postie/internal/arr"
	"github.com/kipsilabs/postie/internal/config"
)

// SetupArrWebhook registers a Postie import webhook with the given arr instance,
// persists the instance (with the returned WebhookID) to config, and saves config.
// webhookURL must be the full URL Postie is reachable at, including the ?apiKey
// query param so arr can authenticate the call.
func (a *App) SetupArrWebhook(ctx context.Context, instance config.ArrInstance, webhookURL string) (config.ArrInstance, error) {
	id, err := arr.SetupWebhook(ctx, instance, webhookURL)
	if err != nil {
		return instance, fmt.Errorf("setting up arr webhook: %w", err)
	}
	instance.WebhookID = id
	instance.Enabled = true

	cfg, err := a.GetConfig()
	if err != nil {
		return instance, err
	}
	cfg.Arr.Instances = append(cfg.Arr.Instances, instance)
	if err := a.SaveConfig(cfg); err != nil {
		return instance, err
	}
	return instance, nil
}

// RemoveArrInstance removes the arr webhook from the app and deletes the
// instance from config. If the webhook cannot be removed from arr (e.g. the
// arr instance is unreachable), the instance is still removed from config.
func (a *App) RemoveArrInstance(ctx context.Context, instanceID string) error {
	cfg, err := a.GetConfig()
	if err != nil {
		return err
	}

	idx := -1
	for i, inst := range cfg.Arr.Instances {
		if inst.ID == instanceID {
			idx = i
			break
		}
	}
	if idx < 0 {
		return fmt.Errorf("arr instance %q not found", instanceID)
	}

	instance := cfg.Arr.Instances[idx]
	if removeErr := arr.RemoveWebhook(ctx, instance); removeErr != nil {
		slog.Warn("could not remove arr webhook from remote app", "instanceID", instanceID, "error", removeErr)
	}

	cfg.Arr.Instances = append(cfg.Arr.Instances[:idx], cfg.Arr.Instances[idx+1:]...)
	return a.SaveConfig(cfg)
}

// GetArrInstances returns all configured arr instances.
func (a *App) GetArrInstances() ([]config.ArrInstance, error) {
	cfg, err := a.GetConfig()
	if err != nil {
		return nil, err
	}
	if cfg.Arr.Instances == nil {
		return []config.ArrInstance{}, nil
	}
	return cfg.Arr.Instances, nil
}

// TestArrConnection verifies the given arr instance is reachable and the API key is valid.
func (a *App) TestArrConnection(ctx context.Context, instance config.ArrInstance) error {
	return arr.TestConnection(ctx, instance)
}
