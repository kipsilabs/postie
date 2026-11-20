-- +goose Up
-- Create initial goqite schema with priority support

-- Create goqite table with priority support
create table if not exists goqite (
  id text primary key default ('m_' || lower(hex(randomblob(16)))),
  created text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
  updated text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
  queue text not null,
  body blob not null,
  timeout text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
  received integer not null default 0,
  priority integer not null default 0
) strict;

-- Create updated timestamp trigger (removed due to goose parsing issues)
-- Will be added in a separate migration if needed

-- Create indexes
create index if not exists goqite_queue_created_idx on goqite (queue, created);
create index if not exists goqite_queue_priority_idx on goqite (queue, priority, created);

-- Table for completed items
create table if not exists completed_items (
  id text primary key,
  path text not null,
  size integer not null,
  priority integer not null default 0,
  nzb_path text not null,
  created_at text not null,
  completed_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
  job_data blob not null
);

create index if not exists completed_items_completed_at_idx on completed_items (completed_at);
create index if not exists completed_items_path_idx on completed_items (path);

-- Table for errored items
create table if not exists errored_items (
	id text primary key,
	path text not null,
	size integer not null,
	priority integer not null default 0,
	error_message text not null,
	created_at text not null,
	errored_at text not null default (strftime('%Y-%m-%dT%H:%M:%fZ')),
	job_data blob not null
);

create index if not exists errored_items_errored_at_idx on errored_items (errored_at);
create index if not exists errored_items_path_idx on errored_items (path);

-- +goose Down
-- Drop all tables and indexes

drop index if exists errored_items_path_idx;
drop index if exists errored_items_errored_at_idx;
drop table if exists errored_items;

drop index if exists completed_items_path_idx;
drop index if exists completed_items_completed_at_idx;
drop table if exists completed_items;

drop index if exists goqite_queue_priority_idx;
drop index if exists goqite_queue_created_idx;
drop table if exists goqite;