-- +goose Up
-- Singleton credentials make first-user registration atomic across processes.
create table web_admin (
  id integer primary key check (id = 1),
  username text not null,
  email text not null default '',
  password_hash text not null
);
create table web_auth_secret (
  id integer primary key check (id = 1),
  secret text not null
);
create table web_sessions (
  id text primary key,
  expires_at integer not null
);
-- +goose Down
drop table web_sessions;
drop table web_auth_secret;
drop table web_admin;
