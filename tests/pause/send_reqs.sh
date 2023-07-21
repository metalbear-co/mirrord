#!/usr/bin/env bash

curl $LOGGER_ADDRESS/log/hi-from-local-app
sleep 1 # if the deployed app was not paused, it will send requests while we're sleeping.
curl $LOGGER_ADDRESS/log/hi-again-from-local-app
