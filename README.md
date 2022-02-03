# mirrord

A [Visual Studio Code](https://code.visualstudio.com/) extension that lets you easily redirect mirrored traffic from your production environment to your development environment.

## Features

* Mirroring API requests from your k8s environment to your debugged process.

## Extension Settings

To enable the extension, add the following fields to the launch.json of your debugged process:

`{
  "mirrord": {
                "pod": "<name of the pod whose traffic to mirror>"
            }
}`

## Requirements

kubectl needs to be running in your development environment.