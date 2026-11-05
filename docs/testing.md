<!-- SPDX-License-Identifier: Apache-2.0 -->
# Manual test checklist

All testing happens in the lab VMs. **Never run a join against a real
production domain.** Snapshot every client VM before each join test.

## Lab

- Windows Server DC with test domain `lab.quietbyte.test`
- Test users, 2–3 GPO drive maps, one shared printer
- An OU for Linux PCs and a delegated "join computers" account
- Client VMs: Ubuntu 24.04, Fedora, Arch

## Milestone 1 — scaffold

- [ ] `cargo build --workspace` succeeds on a clean checkout
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all --check` is clean
- [ ] `scripts/check-spdx.sh` reports no missing headers
- [ ] GitHub Actions CI is green on the pull request
- [ ] `./target/debug/rabita-helper` prints the detected distro family

## Milestone 2 — helper on Ubuntu

_To be written with the milestone._

## Milestone 3 — wizard GUI

_To be written with the milestone._
