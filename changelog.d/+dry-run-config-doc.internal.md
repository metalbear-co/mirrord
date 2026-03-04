Added a release workflow step to build and push the latest config docs to the docs branch.
If there were no schema changes since the "latest" tag, the step will not run.
If the workflow was manually dispatched, it will do a dry run and not push changes.
