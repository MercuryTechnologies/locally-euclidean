# locally-euclidean

This is an implementation of Facebook's [Manifold] blob storage API that Runs On A Computer(tm).
The purpose of this service is to give buck2 somewhere to put its logs, `buck2 rage` output and other similar data that it would ordinarily send to Manifold at Facebook.

- [`buck2 html`](https://github.com/facebook/buck2/blob/ca732304bc0baba82adc3dc5c2ddfebb871df5cc/app/buck2_server_commands/src/html.rs#L44-L51)
- [`buck2 rage`](https://github.com/facebook/buck2/blob/1ac3d816e97e5b069bc4c4a9349be6c80e8c93f0/app/buck2_client/src/commands/rage/manifold.rs#L51-L64)
- [`buck2 debug persist-event-logs`](https://github.com/facebook/buck2/blob/fc110050fc5c383dadefa376b6093ea770d4bcb1/app/buck2_event_log/src/write.rs#L261-L264)

[Manifold]: https://www.youtube.com/watch?v=tddb-zbmnTo

## Why?

Rewriting the buck2 source code to use a different HTTP API is kind of pointless: the used subset of the Manifold API is not that complicated and it's not going to be S3 compatible anyway as it supports (and uses) appends for both multipart upload and uploads of unknown length.
Since we have to write a service and it can't be a truly trivial S3 wrapper, we might as well just implement the Manifold HTTP API.

N.B. There exists a S3 storage class which is appendable, but it [has a limit of 1000 object parts][s3-appends], and any proposed S3-based implementation would require significant rewriting to accommodate that edge case if we find out it hits that limit.
Fixing that edge case would require becoming stateful among other things which introduce much complexity; it would also be necessary to have lifecycle rules, etc, and then one would have to deal with the service not Running On A Computer.
We don't expect to hit the scale where that's necessary with this service, and if we do, the solution is probably to rotate >1 day old data out to S3.

[s3-appends]: https://docs.aws.amazon.com/AmazonS3/latest/userguide/directory-buckets-objects-append.html

## Goals

The goals of this service are:
* Fast to write and deploy
  * Does not cause unexpected hassles once deployed
* Operable: has OpenTelemetry and it's possible to know what it's doing
* Simple
* Store data we care about about as much as build logs (i.e. not very much)
  * Auth is delegated to the proxy, intended to be deployed behind e.g. Tailscale; we do not need to keep these extremely secret
  * Everything in this service is expected to be garbage-collected after a period of time, durability is not that important
* Small scale: it will survive a terabyte of data without any rework, past that we should consider spending a couple days writing a better solution for that
* Runs On A Computer: just needs a postgres, which contains all mutable data including file blobs.

For the reasons of quickness of writing it and the goals of not having to touch it much later, it's written in Rust.

## Functionality

Buckets are statically defined (TODO: configuration mechanism?), and are directories.

TODO: this must change for postgres backend.

### PUT `/v0/write/:filename?bucketName=:bucketName`

This creates a file in the bucket with the given name and returns [201 Created][http201].

Idempotent: if the file already exists with the provided content, [200 OK][http200] is returned.
If the content is not matching, [409 Conflict][http409] is returned.

Takes the `Content-Type` header from the request and if not present, sets it to `text/plain`.
This is what will be returned when browsing the file.

FIXME(in buck2): Add the content-type on upload of files. I don't want a content type sniffer. You don't want a content type sniffer. Let's not build one.

### POST `/v0/append/:filename?bucketName=:bucketName&writeOffset=:writeOffset`

This appends to the file with the given name at the given offset and returns [200][http200], assuming that the given position is at the end of the file.
If the given position is not actually at the end of the file and it also doesn't match the chunk in the given position, [409 Conflict][http409] is returned.

Idempotent: if the uploaded data at the given offset is identical to the data uploaded, returns [200 OK][http200].

[http200]: https://http.cat/status/200
[http201]: https://http.cat/status/201
[http409]: https://http.cat/status/409

### GET `/explore/:bucketName/:filename`

This shows the file at the given path to the browser with the `Content-Type` given on upload.

## Unanswered questions

* Is it semantically acceptable to stream the request body?

  Technically the request could partially finish before dying, and either we have to
  fully buffer the request body in memory (maybe fine) or write it to the
  final file as we go (which means that we don't have atomicity in the
  case of surprise-disconnects). We can also ignore it for now since we don't
  care *that* much about durability since these are just build log files.

  A design change that would fix this is to use copy_file_range to
  leverage modern CoW filesystems such that appending to a file is done by
  appending to a buffer then renaming over the original file. However,
  this breaks inode based locking schemes and would require locking on the
  basis of canonicalized filename. In short: also eww edge cases!

  Alternatively we could use directories for each file and write it as parts;
  that seems simply worse however. Overall this is a reminder that all of this
  stuff is really hard.

# Development

This is a pretty normal Rust project with the exception of oddities relating to sqlx.
If you have a local cargo toolchain it will just work, modulo needing to have a database.

There's a nix and nix-direnv environment provisioned for you, which you can activate with `direnv allow`.

sqlx verifies SQL queries at build time using the `DATABASE_URL` environment variable, the results of which are cached in `.sqlx/` via `cargo sqlx prepare --workspace`.

If you don't want to use a system postgres, the `.envrc` is configured by default to let you use `process-compose up` to start a project-specific postgres server and automatically configure it.

Since we use this caching feature, nix builds do not need a postgres *in the cargo build itself* and can just use temp-postgres for tests.

## Database stuff

You can use the sqlx tools to do migration development:

- **Wipe DB** and run migrations: `sqlx database reset`
- Create a migration: `sqlx migrate add 'initial schema'`

**Currently** (this would be bad practice if the app were larger), migrations are run on application startup and no effort is made to prevent blowing up prod with this.

Don't write migrations that break back-compat for the prior version of the app.
