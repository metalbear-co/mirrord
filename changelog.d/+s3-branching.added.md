Add S3 bucket branching. `{"type": "s3", "source": {"params": {"bucket": "MY_BUCKET_ENV_VAR"}}}`
gives the session a branch S3 bucket, cloned in the provider's cloud. The branch bucket can be seeded empty,
with all objects, or with the objects matching a list of configured regular expressions.
