default:
	@just --list

# Autoformat code (rust, nix, yaml)
fmt:
	cargo fmt
	nixfmt flake.nix
	prettier -w config.example.yaml

# Regenerate src/schema.rs from diesel migrations
schema:
	rm -f diesel.tmp.db
	diesel --database-url diesel.tmp.db migration run
	rm -f diesel.tmp.db
