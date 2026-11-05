.DEFAULT_GOAL := help
.PHONY: help image shell clean nuke build release dist run fmt fmt-check lint test check

# Run containers as the host user so no root-owned files land in the repo.
export HOST_UID := $(shell id -u)
export HOST_GID := $(shell id -g)

# Allocate a TTY only when make itself runs in one (keeps CI and pipes working).
TTY := $(shell [ -t 0 ] || echo -T)
DEV := docker compose --progress quiet run --rm $(TTY) dev

BOLD  := \033[1m
CYAN  := \033[36m
GREEN := \033[32m
RESET := \033[0m
step = @printf '$(GREEN)==>$(RESET) $(BOLD)%s$(RESET)\n' '$(1)'

ARGS ?= detect

help: ## Show this help
	@awk 'BEGIN { FS = ":.*## "; printf "\nUsage:\n  make $(CYAN)<target>$(RESET)\n" } \
		/^##@/ { printf "\n$(BOLD)%s$(RESET)\n", substr($$0, 5) } \
		/^[a-zA-Z_-]+:.*## / { printf "  $(CYAN)%-12s$(RESET) %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo

##@ Environment

image: ## Build the development image
	$(call step,Building dev image)
	@docker compose build

shell: ## Open a shell in the dev container
	@$(DEV) bash

clean: ## Remove build artifacts
	$(call step,Cleaning build artifacts)
	@$(DEV) cargo clean

nuke: ## Remove containers and cache volumes
	$(call step,Removing containers and volumes)
	@docker compose down --volumes --remove-orphans

##@ Build

build: ## Build debug binary
	$(call step,Building debug)
	@$(DEV) cargo build

release: ## Build optimized release binary
	$(call step,Building release)
	@$(DEV) cargo build --release

dist: ## Build release binaries for Linux and Windows into ./dist/
	$(call step,Building dist binaries)
	@$(DEV) sh -c 'set -e; \
		cargo build --release; \
		cargo build --release --target x86_64-pc-windows-gnu; \
		mkdir -p dist; \
		cp /target/release/aip dist/aip; \
		cp /target/x86_64-pc-windows-gnu/release/aip.exe dist/aip.exe'
	@printf '$(GREEN)✔ dist/aip dist/aip.exe$(RESET)\n'

run: ## Run aip with ARGS (default: "detect"), e.g. make run ARGS="detect --json ."
	@$(DEV) cargo run --quiet -- $(ARGS)

##@ Quality

fmt: ## Format code
	$(call step,Formatting)
	@$(DEV) cargo fmt

fmt-check: ## Check formatting
	$(call step,Checking formatting)
	@$(DEV) cargo fmt --check

lint: ## Run clippy with warnings as errors (Linux and Windows targets)
	$(call step,Linting)
	@$(DEV) sh -c 'set -e; \
		cargo clippy --all-targets -- -D warnings; \
		cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings'

test: ## Run tests
	$(call step,Testing)
	@$(DEV) cargo test

check: fmt-check lint test ## Run all quality checks
	@printf '$(GREEN)✔ All checks passed$(RESET)\n'
