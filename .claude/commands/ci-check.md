Run local CI checks and automatically fix issues when possible.

## Execution Steps

1. **Run quick check**: `bash scripts/ci-local.sh quick`
2. **If format fails**: Run `cargo fmt --all` to auto-fix
3. **If clippy fails**: Run `cargo clippy --fix --allow-dirty --allow-staged` to auto-fix what's possible
4. **Re-run checks**: Verify the fixes worked
5. **Report remaining issues**: For complex problems that can't be auto-fixed (safety issues, test failures, manual clippy warnings), explain what needs to be done

## Auto-fixable vs Manual

| Issue Type | Auto-fix | Manual |
|------------|----------|--------|
| Formatting | ✅ `cargo fmt` | - |
| Simple clippy | ✅ `clippy --fix` | - |
| Complex clippy (MutexGuard across await, too many args) | - | ⚠️ Needs code refactor |
| Safety (UTF-8, /tmp, redaction) | - | ⚠️ Needs code changes |
| Test failures | - | ⚠️ Needs investigation |

## Full Check (before merging to main)

If user specifically asks for full check including tests:
```bash
bash scripts/ci-local.sh
```
