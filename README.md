# mirrord

A [Visual Studio Code](https://code.visualstudio.com/) extension that lets you easily redirect mirrored traffic from your production environment to your development environment.

When you start debugging, mirrord will prompt you to select a pod to mirror traffic from. It will then mirror all traffic from that pod to the process you're debugging.

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

The extension uses **kubectl** commands, so it needs to be configured and running on your environment.



icon: Flaticon.com