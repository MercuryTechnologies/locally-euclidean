{
  lib,
  craneLib,
  inputs,
  stdenv,
  rustPlatform,
  podman,
  rust-analyzer,
  sqlx-cli,
  process-compose,
  postgresql_16,
}:
let
  inherit (lib) fileset;
  src = fileset.toSource {
    root = ../..;
    fileset = fileset.unions [
      (fileset.fileFilter ({ hasExt, ... }: hasExt "toml" || hasExt "rs") ../..)
      ../../Cargo.lock
      ../../migrations
      ../../.sqlx
    ];
  };

  version = "${
    builtins.substring 0 8 (inputs.self.lastModifiedDate or inputs.self.lastModified or "19700101")
  }_${inputs.self.shortRev or "dirty"}";

  commonArgsDeps = {
    pname = "locally-euclidean";
    inherit src;
  };
  commonArgs' = commonArgsDeps // {
    # We need postgres for various db stuff in tests.
    nativeBuildInputs = [ postgresql_16 ];
  };

  # Build *just* the cargo dependencies, so we can reuse
  # all of that work (e.g. via cachix) when running in CI
  cargoArtifacts = craneLib.buildDepsOnly commonArgsDeps;

  commonArgs = commonArgs' // {
    inherit cargoArtifacts;
  };

  testRunArgs = commonArgs // {
    doInstallCargoArtifacts = false;
    env = (commonArgs'.env or { }) // {
      RUST_BACKTRACE = 1;
    };
  };

  releaseArgs = commonArgs // {
    inherit version;
    # Don't run tests; we'll do that in a separate derivation.
    doCheck = false;

    passthru = {
      inherit checks devShell;
    };

    meta = {
      mainProgram = "locally-euclidean";
    };
  };

  checks = {
    locally-euclidean-tests = craneLib.cargoTest testRunArgs;
    locally-euclidean-clippy = craneLib.cargoClippy (
      commonArgs
      // {
        cargoClippyExtraArgs = "--all-targets -- --deny warnings";
      }
    );
    locally-euclidean-doc = craneLib.cargoDoc (
      commonArgs
      // {
        cargoDocExtraArgs = "--document-private-items --no-deps --workspace";
        RUSTDOCFLAGS = "--deny warnings";
      }
    );
    locally-euclidean-fmt = craneLib.cargoFmt commonArgs;
    locally-euclidean-audit = craneLib.cargoAudit (
      commonArgs
      // {
        inherit (inputs) advisory-db;
      }
    );
  };

  devShell = craneLib.devShell {
    inherit checks;

    # Make rust-analyzer work
    RUST_SRC_PATH = rustPlatform.rustLibSrc;

    # Extra development tools (cargo and rustc are included by default).
    packages = lib.filter (lib.meta.availableOn stdenv.hostPlatform) [
      rust-analyzer
      sqlx-cli
      process-compose
      podman
    ];
  };
in
craneLib.buildPackage releaseArgs
