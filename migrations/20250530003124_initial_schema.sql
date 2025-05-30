create extension "lo";

-- The usual Mercury timestamps stuff
create or replace function create_timestamps()
  returns trigger as
$$
begin
  new.created_at = now();
  new.updated_at = now();
  return new;
end
$$ language 'plpgsql';

create or replace function update_timestamps()
  returns trigger as
$$
begin
  new.updated_at = now();
  return new;
end
$$ language 'plpgsql';

create table buckets (
    id uuid primary key not null default gen_random_uuid(),
    name text not null,
    created_at timestamp with time zone not null,
    updated_at timestamp with time zone not null
);

create trigger buckets_insert
  before insert
  on buckets
  for each row
execute procedure create_timestamps();

create trigger buckets_update
  before update
  on buckets
  for each row
execute procedure update_timestamps();

create table files (
    id uuid primary key not null default gen_random_uuid(),
    bucket uuid not null references buckets(id),
    filename text not null,
    blob lo not null,
    created_at timestamp with time zone not null,
    updated_at timestamp with time zone not null
);

-- We use the lo module's triggers to make sure that BLOBs are unlinked when the
-- rows are deleted.
create trigger files_delete before update or delete on files
    for each row execute function lo_manage(blob);

create index on files (bucket);
create unique index on files (bucket, filename);

create trigger files_insert
  before insert
  on files
  for each row
execute procedure create_timestamps();

create trigger files_update
  before update
  on files
  for each row
execute procedure update_timestamps();
