{
  description = "F0RTHSP4CE Telegram bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    nix-filter.url = "github:numtide/nix-filter";
    nixpkgs-mozilla.url = "github:mozilla/nixpkgs-mozilla";

    crane.inputs.nixpkgs.follows = "nixpkgs";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
  };

  outputs = inputs@{ self, nixpkgs, crane, flake-parts, nix-filter
    , nixpkgs-mozilla, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems =
        [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      # deadnix: skip (let's keep self', inputs')
      perSystem = { config, self', inputs', pkgs, system, ... }:
        let
          # Need nightly rust for unstable rustfmt features
          rustDev = ((import nixpkgs {
            inherit system;
            overlays = [ (import nixpkgs-mozilla) ];
          }).rustChannelOf {
            date = "2025-02-03";
            channel = "nightly";
            sha256 = "sha256-XzlxUb1J0vSqrrOrVMd1y0RdJEILGA7nUCM1vZevXCU=";
          }).rust.override {
            # rust-src is required for rust-analyzer
            extensions = [ "rust-src" ];
          };
          baseRuntimeDeps = [ pkgs.bash pkgs.imagemagick pkgs.sqlite ];
          allRuntimeDeps = baseRuntimeDeps
            ++ [ residents-admin-table residents-timeline ];
          buildDeps = [ pkgs.openssl pkgs.perl pkgs.pkg-config pkgs.sqlite ];
          pythonDeps = pkgs.python3.withPackages (p: [
            p.pyyaml
            p.telethon
            # TODO: split into dev and runtime deps
            p.mypy
            p.types-pyyaml
          ]);
          residents-admin-table = pkgs.stdenv.mkDerivation {
            name = "f0-residents-admin-table";
            src = ./residents-admin-table.py;
            dontUnpack = true;
            propagatedBuildInputs = [ pythonDeps ];
            installPhase = "install -Dm755 $src $out/bin/$name";
          };
          residents-timeline = pkgs.buildNpmPackage rec {
            name = "residents-timeline";
            src = nix-filter.lib {
              root = ./residents-timeline;
              include = [
                "f0-logo.svg"
                "index.ts"
                "package-lock.json"
                "package.json"
                "tsconfig.json"
              ];
            };
            npmDepsHash = (import ./hashes.nix).residents-timeline;
          };
          revision = self.lastModifiedDate + "-"
            + self.shortRev or self.dirtyShortRev or "unknown";

          craneLib = crane.mkLib pkgs;
        in rec {
          formatter = pkgs.nixfmt;
          packages.default = packages.f0bot;

          packages.f0bot = pkgs.writeScriptBin "f0bot" ''
            #!${pkgs.stdenv.shell}
            export PATH=${pkgs.lib.makeBinPath allRuntimeDeps}:$PATH
            exec ${packages.f0bot-unwrapped}/bin/f0bot \
              ---set-revision ${revision} "$@"
          '';

          packages.f0bot-unwrapped = craneLib.buildPackage {
            src = nix-filter.lib {
              root = ./.;
              include =
                [ "src" "Cargo.toml" "Cargo.lock" "config.example.yaml" ];
            };
            # Tests require network access, which is not allowed in the build
            doCheck = false;
            nativeBuildInputs = buildDeps;
          };

          packages.residents-timeline = residents-timeline;

          packages.residents-admin-table = residents-admin-table;

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

          devShells.default = pkgs.mkShell.override {
            # Use mold for faster linking
            stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;
          } {
            buildInputs = [
              pythonDeps
              rustDev
              (pkgs.diesel-cli.override {
                postgresqlSupport = false;
                mysqlSupport = false;
              })
              pkgs.bun
              pkgs.just
              pkgs.mold
              pkgs.nodejs
              pkgs.prefetch-npm-deps

              # Linters and formatters (see Justfile)
              pkgs.deadnix
              pkgs.nixfmt
              pkgs.nodePackages.prettier
              pkgs.ruff
              pkgs.statix
              pkgs.cargo-deny
            ] ++ buildDeps ++ baseRuntimeDeps;
          };
        };
    };
}
