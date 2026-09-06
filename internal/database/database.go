package database

import (
	"context"
	"database/sql"
	"fmt"
	"log/slog"

	"github.com/kipsilabs/postie/internal/config"
	_ "github.com/mattn/go-sqlite3"
)

// Database represents a database connection with migration capabilities
type Database struct {
	DB     *sql.DB
	dbPath string
}

// New creates a new database connection
func New(ctx context.Context, cfg config.DatabaseConfig) (*Database, error) {
	dbPath := cfg.DatabasePath
	if dbPath == "" {
		dbPath = "postie.db"
	}

	slog.InfoContext(ctx, fmt.Sprintf("Using %s database at %s", cfg.DatabaseType, dbPath))

	// For now, only SQLite is fully implemented
	if cfg.DatabaseType != "sqlite" {
		return nil, fmt.Errorf("database type %s is not yet implemented, please use 'sqlite'", cfg.DatabaseType)
	}

	db, err := sql.Open("sqlite3", dbPath+"?_journal=WAL&_timeout=5000&_fk=true")
	if err != nil {
		return nil, err
	}

	// Configure connection pool
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)

	return &Database{
		DB:     db,
		dbPath: dbPath,
	}, nil
}

// Close closes the database connection
func (d *Database) Close() error {
	if d.DB != nil {
		return d.DB.Close()
	}
	return nil
}

// GetMigrationRunner returns a new migration runner for this database
func (d *Database) GetMigrationRunner() *MigrationRunner {
	return NewMigrationRunner(d.DB)
}

// EnsureMigrationCompatibility ensures the database is compatible with migrations
func (d *Database) EnsureMigrationCompatibility() error {
	migrationRunner := d.GetMigrationRunner()
	return migrationRunner.EnsureMigrationCompatibility()
}
