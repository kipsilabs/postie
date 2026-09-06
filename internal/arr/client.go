package arr

import (
	"context"
	"fmt"

	"golift.io/starr"
	"golift.io/starr/lidarr"
	"golift.io/starr/radarr"
	"golift.io/starr/readarr"
	"golift.io/starr/sonarr"

	"github.com/kipsilabs/postie/internal/config"
)

const notificationName = "Postie"

// SetupWebhook registers a Postie webhook notification with the target *arr
// app using its native API. Returns the notification ID created in the arr app,
// which must be saved to ArrInstance.WebhookID for later removal.
// webhookURL must be the full Postie URL including the ?apiKey query param.
func SetupWebhook(ctx context.Context, instance config.ArrInstance, webhookURL string) (int64, error) {
	cfg := &starr.Config{
		APIKey: instance.APIKey,
		URL:    instance.URL,
	}

	fields := []*starr.FieldInput{
		{Name: "url", Value: webhookURL},
		{Name: "method", Value: 2}, // 2 = POST
	}

	switch instance.Type {
	case config.ArrTypeRadarr:
		return setupRadarrWebhook(ctx, radarr.New(cfg), fields)
	case config.ArrTypeSonarr:
		return setupSonarrWebhook(ctx, sonarr.New(cfg), fields)
	case config.ArrTypeLidarr:
		return setupLidarrWebhook(ctx, lidarr.New(cfg), fields)
	case config.ArrTypeReadarr:
		return setupReadarrWebhook(ctx, readarr.New(cfg), fields)
	default:
		return 0, fmt.Errorf("unsupported arr type: %q", instance.Type)
	}
}

func setupRadarrWebhook(ctx context.Context, client *radarr.Radarr, fields []*starr.FieldInput) (int64, error) {
	out, err := client.AddNotificationContext(ctx, &radarr.NotificationInput{
		Name:           notificationName,
		OnDownload:     true,
		OnUpgrade:      true,
		Implementation: "Webhook",
		ConfigContract: "WebhookSettings",
		Fields:         fields,
	})
	if err != nil {
		return 0, fmt.Errorf("radarr AddNotification: %w", err)
	}
	return out.ID, nil
}

func setupSonarrWebhook(ctx context.Context, client *sonarr.Sonarr, fields []*starr.FieldInput) (int64, error) {
	out, err := client.AddNotificationContext(ctx, &sonarr.NotificationInput{
		Name:           notificationName,
		OnDownload:     true,
		OnUpgrade:      true,
		Implementation: "Webhook",
		ConfigContract: "WebhookSettings",
		Fields:         fields,
	})
	if err != nil {
		return 0, fmt.Errorf("sonarr AddNotification: %w", err)
	}
	return out.ID, nil
}

func setupLidarrWebhook(ctx context.Context, client *lidarr.Lidarr, fields []*starr.FieldInput) (int64, error) {
	out, err := client.AddNotificationContext(ctx, &lidarr.NotificationInput{
		Name:            notificationName,
		OnReleaseImport: true, // Lidarr uses OnReleaseImport, not OnDownload
		OnUpgrade:       true,
		Implementation:  "Webhook",
		ConfigContract:  "WebhookSettings",
		Fields:          fields,
	})
	if err != nil {
		return 0, fmt.Errorf("lidarr AddNotification: %w", err)
	}
	return out.ID, nil
}

func setupReadarrWebhook(ctx context.Context, client *readarr.Readarr, fields []*starr.FieldInput) (int64, error) {
	out, err := client.AddNotificationContext(ctx, &readarr.NotificationInput{
		Name:            notificationName,
		OnReleaseImport: true, // Readarr uses OnReleaseImport, not OnDownload
		OnUpgrade:       true,
		Implementation:  "Webhook",
		ConfigContract:  "WebhookSettings",
		Fields:          fields,
	})
	if err != nil {
		return 0, fmt.Errorf("readarr AddNotification: %w", err)
	}
	return out.ID, nil
}

// RemoveWebhook deletes the Postie notification from the arr app using the
// stored WebhookID. No-op if WebhookID is 0.
func RemoveWebhook(ctx context.Context, instance config.ArrInstance) error {
	if instance.WebhookID == 0 {
		return nil
	}

	cfg := &starr.Config{
		APIKey: instance.APIKey,
		URL:    instance.URL,
	}

	switch instance.Type {
	case config.ArrTypeRadarr:
		return radarr.New(cfg).DeleteNotificationContext(ctx, instance.WebhookID)
	case config.ArrTypeSonarr:
		return sonarr.New(cfg).DeleteNotificationContext(ctx, instance.WebhookID)
	case config.ArrTypeLidarr:
		return lidarr.New(cfg).DeleteNotificationContext(ctx, instance.WebhookID)
	case config.ArrTypeReadarr:
		return readarr.New(cfg).DeleteNotificationContext(ctx, instance.WebhookID)
	default:
		return fmt.Errorf("unsupported arr type: %q", instance.Type)
	}
}

// TestConnection verifies the arr instance is reachable and the API key is valid.
func TestConnection(ctx context.Context, instance config.ArrInstance) error {
	cfg := &starr.Config{
		APIKey: instance.APIKey,
		URL:    instance.URL,
	}

	var err error
	switch instance.Type {
	case config.ArrTypeRadarr:
		_, err = radarr.New(cfg).GetSystemStatusContext(ctx)
	case config.ArrTypeSonarr:
		_, err = sonarr.New(cfg).GetSystemStatusContext(ctx)
	case config.ArrTypeLidarr:
		_, err = lidarr.New(cfg).GetSystemStatusContext(ctx)
	case config.ArrTypeReadarr:
		_, err = readarr.New(cfg).GetSystemStatusContext(ctx)
	default:
		return fmt.Errorf("unsupported arr type: %q", instance.Type)
	}

	if err != nil {
		return fmt.Errorf("arr connection test failed: %w", err)
	}
	return nil
}
