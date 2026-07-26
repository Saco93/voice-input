# Project Agent Rules

## Branch Protection

- Never bypass the required `validate` status check. Do not use admin
  privileges, `--force`, or any other mechanism to push to `main` while
  the `validate` check is pending, missing, or failing.
- Always wait until the `validate` check (CI job in
  `.github/workflows/ci.yml`) passes on the commit before pushing to
  `main`, and follow the branch protection rules at all times.
- If a push is rejected because `validate` has not completed or failed,
  stop, fix the underlying issue, and push again only after the check
  succeeds.
