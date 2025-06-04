{
  lib,
  craneLib,
  stdenv,
  vmTools,
  e2fsprogs,
  shadow,
  inputs,
  rustPlatform,
  rust-analyzer,
  sqlx-cli,
  process-compose,
  postgresql,
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

  commonArgsDeps = {
    pname = "locally-euclidean";
    inherit src;
  };
  commonArgs' = commonArgsDeps // {
    # We need postgres for various db stuff in tests.
    nativeBuildInputs = [ postgresql ];
  };

  # Build *just* the cargo dependencies, so we can reuse
  # all of that work (e.g. via cachix) when running in CI
  cargoArtifacts = craneLib.buildDepsOnly commonArgsDeps;

  commonArgs = commonArgs' // {
    inherit cargoArtifacts;
  };

  # XXX: This is a Big Hammer workaround for Lix not supporting xattr usage in
  # the build sandbox for bad reasons.
  # https://git.lix.systems/lix-project/lix/issues/838
  maybeRunInLinuxVM =
    drv:
    if stdenv.isLinux then
      vmTools.runInLinuxVM (
        drv.overrideAttrs (old: {
          # Seems that the default memory size is not large enough.
          # This is megabytes.
          memSize = 6072;

          preBuild = ''
            ${old.preBuild or ""}
            ${lib.getExe' shadow "useradd"} -m postgres
            env
          '';

          # FIXME(jadel): tests are currently, frustratingly, rebuilt every time inside
          # the VM anyway, due to the embedded migrations saving absolute file paths
          # into cargo's incremental state somewhere such that it gets invalidated if
          # the source directory moves.
          #
          # The fix to this, though, is to just get rid of the VM and the split when we
          # get rid of the file backend, so, for now, whatever.
          #
          # This debug output is left in to make the problem's cause obvious.
          env = (old.env or { }) // {
            CARGO_LOG = "cargo::core::compiler::fingerprint=info";
          };

          # Provide a rootfs that is not itself on tmpfs.
          mountDisk = true;
          preVM = ''
            truncate -s 6072M ./disk-image.img
            ${e2fsprogs}/bin/mkfs.ext4 ./disk-image.img
            diskImage=$PWD/disk-image.img
          '';
        })
      )
    else
      drv;

  testArgs = commonArgs // {
    # Don't run tests, just build them
    doInstallCargoArtifacts = true;
    cargoTestExtraArgs = "--no-run";
  };
  testBuild = craneLib.cargoTest testArgs;

  testRunArgs = commonArgs // {
    cargoArtifacts = testBuild;
    env = (commonArgs'.env or { }) // {
      RUST_BACKTRACE = 1;
    };
  };

  releaseArgs = commonArgs // {
    # Don't run tests; we'll do that in a separate derivation.
    doCheck = false;
    passthru = {
      inherit checks devShell;
    };
  };

  checks = {
    locally-euclidean-tests-compile = testBuild;
    locally-euclidean-tests = maybeRunInLinuxVM (craneLib.cargoTest testRunArgs);
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
    packages = [
      rust-analyzer
      sqlx-cli
      process-compose
    ];
  };
in
craneLib.buildPackage releaseArgs
