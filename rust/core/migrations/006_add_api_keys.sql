-- +goose Up
-- Single-row table holding the HTTP API key. The id column is fixed at 1 so
-- upserts replace the existing row in place when the key is regenerated.

create table if not exists api_keys (
  id integer primary key check (id = 1),
  key text not null,
  created_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ'))
);

-- +goose Down
drop table if exists api_keys;
