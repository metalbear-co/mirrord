SQS splitting now supports filtering by jq programs. A jq program runs against a json of each message and if it returns true, the messaged is filtered.
