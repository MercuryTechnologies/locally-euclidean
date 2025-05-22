{
  lib,
  craneLib,
  stdenv,
  vmTools,
  e2fsprogs,
  inputs,
  rustPlatform,
  rust-analyzer,
}:
let
  src = lib.cleanSourceWith {
    src = craneLib.path (inputs.self.outPath);
    filter = craneLib.filterCargoSources;
  };

  commonArgs' = {
    pname = "locally-euclidean";
    inherit src;
  };

  # Build *just* the cargo dependencies, so we can reuse
  # all of that work (e.g. via cachix) when running in CI
  cargoArtifacts = craneLib.buildDepsOnly commonArgs';

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

  testRunArgs = commonArgs' // {
    cargoArtifacts = testBuild;
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
    ];
  };
in
craneLib.buildPackage releaseArgs
