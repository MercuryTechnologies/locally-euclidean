{
  lib,
  craneLib,
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

  releaseArgs = commonArgs // {
    # Don't run tests; we'll do that in a separate derivation.
    doCheck = false;
    passthru = {
      inherit checks devShell;
    };
  };

  checks = {
    locally-euclidean-tests = craneLib.cargoTest commonArgs;
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
