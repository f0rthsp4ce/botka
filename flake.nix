{
  description = "F0RTHSP4CE Telegram bot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    nix-filter.url = "github:numtide/nix-filter";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils"; # Added flake-utils for convenience
  };

  outputs = { self, nixpkgs, crane, nix-filter, rust-overlay, flake-utils, ... }:
    # Using flake-utils for perSystem boilerplate
    flake-utils.lib.eachDefaultSystem (system:
      let
        # Import nixpkgs with the rust-overlay applied
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # Use rust-overlay to get the specific nightly toolchain
        rustDev = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [
            "rust-src"
          ];
        };

        # Define base runtime dependencies
        baseRuntimeDeps = [ pkgs.bash pkgs.imagemagick pkgs.sqlite ];

        # Define Python dependencies
        pythonDeps = pkgs.python3.withPackages (p: [
          p.pyyaml
          p.telethon
          # TODO: split into dev and runtime deps
          p.mypy
          p.types-pyyaml
        ]);

        # Define the residents-admin-table package
        residents-admin-table = pkgs.stdenv.mkDerivation {
          name = "f0-residents-admin-table";
          src = ./residents-admin-table.py;
          dontUnpack = true;
          propagatedBuildInputs = [ pythonDeps ];
          installPhase = "install -Dm755 $src $out/bin/$name";
        };

        # Define the residents-timeline package
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
          nativeBuildInputs = [ pkgs.nodejs ];
        };

        # Combine all runtime dependencies
        allRuntimeDeps = baseRuntimeDeps ++ [ residents-admin-table residents-timeline ];

        # Define build dependencies for the Rust package
        buildDeps = [ pkgs.openssl pkgs.perl pkgs.pkg-config pkgs.sqlite ];

        # Calculate the revision string
        revision = self.lastModifiedDate or "nodate" + "-"
          + self.shortRev or self.dirtyShortRev or "unknown";

        # Initialize crane library using the standard function
        craneLib = crane.mkLib pkgs; # Use mkLib provided by crane flake

      in rec {
        # Formatter for nix files
        formatter = pkgs.nixfmt-rfc-style;

        # Default package points to the main bot package
        packages.default = packages.f0bot;

        # Wrapper script for the bot executable
        packages.f0bot = pkgs.writeScriptBin "f0bot" ''
          #!${pkgs.stdenv.shell}
          # Add runtime dependencies to PATH
          export PATH=${pkgs.lib.makeBinPath allRuntimeDeps}:$PATH
          # Execute the unwrapped bot binary, passing revision and arguments
          exec ${packages.f0bot-unwrapped}/bin/f0bot \
            --set-revision ${revision} "$@"
        '';

        # Build the main Rust bot package using crane
        packages.f0bot-unwrapped = craneLib.buildPackage {
          src = nix-filter.lib {
            root = ./.;
            include =
              [ "src" "Cargo.toml" "Cargo.lock" "config.example.yaml" ];
          };
          # Pass the specific rust toolchain to crane here
          rustToolchain = rustDev;
          # Tests require network access, which is not allowed in the build sandbox
          doCheck = false;
          # Use the build dependencies defined above
          nativeBuildInputs = buildDeps;
        };

        # Expose the residents-timeline package
        packages.residents-timeline = residents-timeline;

        # Expose the residents-admin-table package
        packages.residents-admin-table = residents-admin-table;

        # Build the Docker image
        packages.image = pkgs.dockerTools.buildImage {
          name = "f0bot";
          tag = revision; # Use revision for the tag for better tracking
          copyToRoot = pkgs.buildEnv {
            name = "image-root";
            paths = [ packages.f0bot pkgs.cacert ];
            pathsToLink = [ "/bin" "/etc" ];
          };
          config = {
            Cmd = [ "/bin/f0bot" ];
            # ExposedPorts = { "8080/tcp" = {}; };
            # Env = [ "SOME_VAR=value" ];
          };
        };

        # Development shell environment
        devShells.default = pkgs.mkShell {
          # Inputs required for development
          inputsFrom = [ packages.f0bot-unwrapped ]; # Inherit inputs from the package build
          # Additional development tools
          nativeBuildInputs = [
            pythonDeps
            # rustDev is now implicitly included via the rustToolchain argument in f0bot-unwrapped
            # but we still need RUST_SRC_PATH for rust-analyzer, so keep rustDev here too,
            # or reference it directly in shellHook. Let's keep it simple:
            rustDev
            (pkgs.diesel-cli.override {
              postgresqlSupport = false;
              mysqlSupport = false;
            })
            pkgs.bun
            pkgs.just # Task runner
            pkgs.mold # Faster linker
            pkgs.nodejs # For residents-timeline dev
            pkgs.prefetch-npm-deps # For updating npmDepsHash

            # Linters and formatters (ensure these match your Justfile tasks)
            pkgs.deadnix
            pkgs.nixfmt-rfc-style # Or pkgs.nixfmt
            pkgs.nodePackages.prettier
            pkgs.ruff # Python linter
            pkgs.statix # Nix linter/formatter
            pkgs.cargo-deny

            pkgs.openssl
          ] ++ baseRuntimeDeps; # Include base runtime deps in the shell

          # Environment variables for the shell
          shellHook = ''
            # Add any custom shell initialisation here
            echo "F0RTHSP4CE Bot Dev Shell Ready!"
            # Ensure rust-analyzer finds the source code for the standard library
            export RUST_SRC_PATH="${rustDev}/lib/rustlib/src/rust/library"
            # Add other exports or aliases if needed
          '';
        };
      }
    );
}
