default:
    @just --list

# Run a session in the current repository with debug phase timings visible
run *args:
    cargo run --release -- -v run {{args}}

# Profile a pithos invocation into flamegraph.svg (defaults to `run`)
flamegraph *args='run':
    cargo flamegraph --profile profiling --bin pithos --output flamegraph.svg -- {{args}}
