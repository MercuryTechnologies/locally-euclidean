# Check that this flake is valid and run the test suite with `nix flake check`.
#
# Enter a development shell with `nix develop`.
# Build with `nix build`, which will generate `result/bin/locally-euclidean`.
{
  description = "Facebook Manifold API implementation";

  # Update these inputs with `nix flake update`.
  # Create missing entries in `flake.lock` with `nix flake lock`.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    crane.url = "github:ipetkov/crane";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };
  };

  nixConfig = {
    extra-substituters = [ "https://cache.garnix.io" ];
    extra-trusted-substituters = [ "https://cache.garnix.io" ];
    extra-trusted-public-keys = [ "cache.garnix.io:CTFPyKSLcx5RMJKfLo5EEPUObbA78b0YQ2DTCJXqr9g=" ];
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      crane,
      rust-overlay,
      advisory-db,
      ...
    }:
    let
      eachSystem = nixpkgs.lib.genAttrs [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];

      makePkgs =
        args:
        import nixpkgs (
          args
          // {
            overlays = [
              rust-overlay.overlays.default
              (final: prev: {
                rustToolchain = final.pkgsBuildHost.rust-bin.stable.latest.default.override {
                  targets = final.lib.optionals final.stdenv.targetPlatform.isDarwin [
                    "x86_64-apple-darwin"
                    "aarch64-apple-darwin"
                  ];
                  extensions = [ "llvm-tools-preview" ];
                };

                craneLib = (crane.mkLib final).overrideToolchain final.rustToolchain;
              })
            ];
          }
        );
    in
    {
      _pkgs = eachSystem (localSystem: makePkgs { inherit localSystem; });

      localPkgs = eachSystem (
        localSystem: self._pkgs.${localSystem}.callPackage ./nix/makePackages.nix { inherit inputs; }
      );

      packages = eachSystem (
        localSystem:
        let
          inherit (nixpkgs) lib;
          localPkgs = self.localPkgs.${localSystem};
          pkgs = self._pkgs.${localSystem};
          inherit (localPkgs) locally-euclidean;
        in
        (lib.filterAttrs (
          name: value: lib.isDerivation value && lib.meta.availableOn pkgs.stdenv.hostPlatform value
        ) localPkgs)
        // {
          inherit locally-euclidean;
          default = locally-euclidean;
        }
      );

      checks = eachSystem (
        system:
        builtins.removeAttrs self.packages.${system}.default.checks
          # CI and `nix flake check` complain that these are not derivations.
          [
            "override"
            "overrideDerivation"
          ]
      );

      devShells = eachSystem (system: {
        default = self.packages.${system}.default.devShell;
      });
    };
}
