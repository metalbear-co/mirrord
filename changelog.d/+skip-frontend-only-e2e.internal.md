Skip CI runner and agent image builds together with end-to-end tests for frontend-only changes so e2e jobs cannot wait
for artifacts that will not be produced.
