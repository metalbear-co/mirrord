#!/usr/bin/env bash

curl mirrord-tests-http-logger/log/hi-from-local-app
sleep 1 # if the deployed app was not paused, it will send requests while we're sleeping.
curl mirrord-tests-http-logger/log/hi-again-from-local-app
