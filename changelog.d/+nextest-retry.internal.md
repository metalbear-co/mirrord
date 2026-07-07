Added step to e2e workflow to install nextest with configuration at `.config/nextest.toml`. Failed
e2e tests will now retry twice after failing before failing the whole CI.
