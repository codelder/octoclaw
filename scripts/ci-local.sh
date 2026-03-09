#!/usr/bin/env bash
# Local CI checks that mirror GitHub Actions workflow.
#
# Usage:
#   bash scripts/ci-local.sh           # Run all checks
#   bash scripts/ci-local.sh quick     # Skip tests, run format + clippy only
#   bash scripts/ci-local.sh format    # Run formatting check only
#   bash scripts/ci-local.sh clippy    # Run clippy checks only
#   bash scripts/ci-local.sh safety    # Run safety checks only
#   bash scripts/ci-local.sh test      # Run tests only
#
# Exit codes:
#   0 - All checks passed
#   1 - One or more checks failed

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

FAILED=0

section() {
    echo ""
    echo -e "${YELLOW}=== $1 ===${NC}"
    echo ""
}

success() {
    echo -e "${GREEN}✓ $1${NC}"
}

fail() {
    echo -e "${RED}✗ $1${NC}"
    FAILED=1
}

run_format_check() {
    section "Formatting Check"

    if cargo fmt --all -- --check; then
        success "Code formatting is correct"
    else
        fail "Code formatting issues found. Run 'cargo fmt' to fix."
        echo ""
        echo "  Hint: cargo fmt --all"
    fi
}

run_clippy_check() {
    section "Clippy Lints"

    local CLIPPY_FAILED=0

    echo "Checking with all features..."
    if cargo clippy --all --benches --tests --examples --all-features -- -D warnings 2>&1; then
        success "Clippy (all features) passed"
    else
        fail "Clippy (all features) failed"
        CLIPPY_FAILED=1
    fi

    echo ""
    echo "Checking with default features..."
    if cargo clippy --all --benches --tests --examples -- -D warnings 2>&1; then
        success "Clippy (default features) passed"
    else
        fail "Clippy (default features) failed"
        CLIPPY_FAILED=1
    fi

    echo ""
    echo "Checking with libsql only..."
    if cargo clippy --all --benches --tests --examples --no-default-features --features libsql -- -D warnings 2>&1; then
        success "Clippy (libsql only) passed"
    else
        fail "Clippy (libsql only) failed"
        CLIPPY_FAILED=1
    fi

    if [ "$CLIPPY_FAILED" -eq 1 ]; then
        FAILED=1
        echo ""
        echo "  Hint: cargo clippy --fix --allow-dirty (for auto-fixable issues)"
    fi
}

run_safety_check() {
    section "Safety Checks"

    if bash scripts/pre-commit-safety.sh; then
        success "Safety checks passed"
    else
        fail "Safety checks found issues"
    fi
}

run_tests() {
    section "Running Tests"

    if cargo test --all-features; then
        success "All tests passed"
    else
        fail "Some tests failed"
    fi
}

# Main logic
MODE="${1:-all}"

case "$MODE" in
    quick)
        run_format_check
        run_clippy_check
        run_safety_check
        ;;
    format)
        run_format_check
        ;;
    clippy)
        run_clippy_check
        ;;
    safety)
        run_safety_check
        ;;
    test)
        run_tests
        ;;
    all|*)
        run_format_check
        run_clippy_check
        run_safety_check
        run_tests
        ;;
esac

# Summary
echo ""
echo "================================"
if [ "$FAILED" -eq 0 ]; then
    echo -e "${GREEN}All checks passed!${NC}"
    exit 0
else
    echo -e "${RED}Some checks failed. Please fix the issues above.${NC}"
    exit 1
fi
