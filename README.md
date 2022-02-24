# mirrord

A [Visual Studio Code](https://code.visualstudio.com/) extension that lets you easily mirror traffic from your production environment to your development environment.

When you start debugging, mirrord will prompt you to select a pod to mirror traffic from. It will then mirror all traffic from that pod to the process you're debugging.

## How to use

* Start debugging your project.
* Click "Start mirrord" on the status bar.
* Choose pod to mirror traffic from.
* Click stop when you wish to finish (or stop debugging).

## Demo
![Demo!](https://i.imgur.com/LujQb1u.gif)


## Extension Settings

To enable the extension, add the following fields to the launch.json of your debugged process:

`{
  "mirrord": {
                "pod": "<name of the pod whose traffic to mirror>"
            }
}`
