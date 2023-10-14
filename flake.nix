{
  description = "F0RTHSP4CE Telegram bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    naersk.url = "github:nix-community/naersk";
    nix-filter.url = "github:numtide/nix-filter";
    nixpkgs-mozilla.url = "github:mozilla/nixpkgs-mozilla";

    naersk.inputs.nixpkgs.follows = "nixpkgs";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
  };

  outputs = inputs@{ self, nixpkgs, flake-parts, naersk, nix-filter
    , nixpkgs-mozilla, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems =
        [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      perSystem = { config, self', inputs', pkgs, system, ... }:
        let
          # Need nightly rust for unstable rustfmt features
          rustDev = ((import nixpkgs {
            inherit system;
            overlays = [ (import nixpkgs-mozilla) ];
          }).rustChannelOf {
            date = "2023-10-14";
            channel = "nightly";
            sha256 = "sha256-pFboSWLjOvUnWPnV2nzQ8eUSLmbzmHiuRqFdSZbPdoI=";
          }).rust.override {
            # rust-src is required for rust-analyzer
            extensions = [ "rust-src" ];
          };
        in rec {
          formatter = pkgs.nixfmt;
          packages.default = packages.f0bot;

          packages.f0bot = (pkgs.callPackage naersk { }).buildPackage {
            src = nix-filter.lib {
              root = ./.;
              include = [ "src" "Cargo.toml" "Cargo.lock" ];
            };
            buildInputs = [ pkgs.perl ];
          };

          packages.image = pkgs.dockerTools.buildImage {
            name = "f0bot";
            tag = "latest";
            copyToRoot = pkgs.buildEnv {
              name = "image-root";
              paths = [ packages.f0bot pkgs.cacert ];
              pathsToLink = [ "/bin" "/etc" ];
            };
            config.Cmd = [ "/bin/f0bot" ];
          };

          devShells.default = pkgs.mkShell {
            # Use mold for faster linking
            env = { RUSTFLAGS = "-Clink-args=-fuse-ld=mold"; };
            buildInputs = [
              rustDev
              (pkgs.diesel-cli.override {
                postgresqlSupport = false;
                mysqlSupport = false;
              })
              pkgs.mold
              pkgs.nixfmt
              pkgs.perl
              pkgs.sqlite
            ];
          };
        };
    };
}
