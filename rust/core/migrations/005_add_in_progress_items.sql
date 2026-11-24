-- +goose Up
-- Track items currently being processed so they can be recovered if the app crashes

create table if not exists in_progress_items (
  id text primary key,
  path text not null,
  size integer not null,
  priority integer not null default 0,
  created_at text not null,
  started_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
  job_data blob not null
);

create index if not exists in_progress_items_path_idx on in_progress_items (path);

-- +goose Down
drop index if exists in_progress_items_path_idx;
drop table if exists in_progress_items;
