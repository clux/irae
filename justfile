open := if os() == "macos" { "open" } else { "xdg-open" }

[private]
default:
  @just --list --unsorted

fmt:
  cargo +nightly fmt
lint:
  cargo clippy
doc:
  RUSTDOCFLAGS="--cfg docsrs" cargo +nightly doc --all-features --no-deps --open
test:
  cargo test --lib

coverage:
  cargo tarpaulin --out=Html --output-dir=.
  {{open}} tarpaulin-report.html

release:
  cargo release minor --execute
