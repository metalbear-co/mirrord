# How to Debug a Node.js Microservice

In this guide, we’ll cover how to debug a Node.js microservice running in a Kubernetes environment using mirrord. You’ll learn how to set up and use mirrord with the VSCode IDE and the command line.

Tip: You can use mirrord to debug, test, and troubleshoot your applications locally with Kubernetes context, without needing to build or deploy each time.

## Common debugging techniques for microservices in Node.js

It can be cumbersome to debug microservices on Kubernetes. The lack of a debugging workflow for applications with multiple runtime dependencies in the context of Kubernetes makes it even harder. Why does it even make it harder? The following are common ways of debugging microservices with strict runtime environment dependencies:

### Continuous Deployment

Build a container image and deploy it to a Kubernetes cluster dedicated to testing or staging. The iterative process of building, deploying, and testing is resource-intensive and time-consuming, especially for testing frequent code changes.

### Log Analysis

One of the most common ways to understand the application behavior in a cluster is by analyzing logs. Adding extra logs to extract runtime information on the application is very common. Collecting and analyzing logs from different services can be effective but it isn’t the best real-time debugging solution.

### Remote Debugging

Developers can use remote debugging tools built into IDEs like VSCode to attach to processes already running in a Kubernetes cluster. While this allows real-time code inspection and interaction, it still requires heavy overhead from the IDE and a separate debug configuration for the deployment, which can potentially affect the application's performance while debugging.

The above methods can be used by themselves or they can be used together.

## Challenges of debugging Node.js microservices in Kubernetes

Debugging effectively within a Kubernetes context is the biggest challenge of working with Kubernetes. The build and release loop of the application can be short, but the process slows down development significantly. Nothing beats the ease and speed of debugging applications locally.

## Introduction to debugging Node.js microservices with mirrord

With mirrord, we don’t have to think of building and releasing our applications for debugging. We can run our applications locally, and mirrord will make sure to have your locally running process in the context of Kubernetes. Context mirroring for processes allows your process to run locally and consume the resources of a remote resource. 

### Workload to process context mirroring

To achieve this, inputs from a Kubernetes workload (eg: a Pod) are mirrored to a locally running process. The process in question here today is a Node.js process. Let’s see how we can mirror inputs for our locally running Node.js application using mirrord and pipe these outputs back to Kubernetes. This will create a tighter feedback loop effectively allowing you to debug faster without the downsides of the common debugging techniques we discussed above.

### Sample application setup

In the example below, our Node.js application will run locally. It will need to have the network information and environment of a Kubernetes Pod to debug. This Kubernetes Pod is running as part of a staging application deployment and will be our mirroring target.

Let’s get started with some prerequisites by setting up a test cluster and deploying our mirroring target.

## Prerequisites

Set up the Kubernetes cluster to test our application setup.

1. Start an instance of a development cluster like minikube, k3d, kind, etc. We are using minikube here.

```bash
minikube start
```


2. Clone the repo with the sample Node.js application.

```bash
git clone https://github.com/waveywaves/mirrord-nodejs-debug-example
cd mirrord-nodejs-debug-example
```

3. Deploy our application, which will act as our staging environment.

```bash
kubectl create -f ./kube
```

![alt text](how-to-debug-a-nodejs-microservice/kubectl-create-f-kube.png)


Once the above is deployed let’s use the following command to get access to the application endpoint so we can see what it looks like.

```bash
minikube service nodejs-guestbook
```

The above minikube service command automatically sets up a port forwarding session to the specified service and opens it in the default web browser. With the tunnel to our microservice setup, our application architecture now looks like this. 


![alt text](how-to-debug-a-nodejs-microservice/architecture-diagram-without-mirrord.png)

Once we run the minikube service command, we get output which looks something like this. 


![alt text](how-to-debug-a-nodejs-microservice/minikube-service-nodejs-guestbook.png)


Now we have access to the Guestbook application on http://localhost:57485.
Let’s access this URL from the browser.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-6-58-05pm.png)


We have our staging application deployed now. Let’s run the microservice with mirrord now. This will allow us to run the local Node.js application in the context of Kubernetes without having to build and deploy it over and over again for testing. 

## Debug the Guestbook application with VSCode and the VSCode mirrord extension

In this section of the guide, we are going to use the VSCode mirrord extension to help debug the Node.js application. If you would like to see how we can do the same in the CLI, go to this section of the guide.

The application in question is Guestbook, a simple note-taking app written in Node.js with support for storing notes in Redis. The source code for the test application is available on GitHub at https://github.com/waveywaves/mirrord-nodejs-debug-example. We will use it as a follow-along Node.js application for debugging with mirrord.

## Setup VSCode with the mirrord extension

To get started, install mirrord in VSCode.

### Extension installation

You can install the plugin by searching for mirrord in the extensions.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-00-07pm.png)


After mirrord is installed, you will see a mirrord button in the bottom left corner of your VSCode instance.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-00-36pm.png)


### mirrord configuration

Let’s add a new config file for mirrord, which we can use with VSCode for debugging. The configuration below contains the target deployment from where we need to mirror the context. 

```json

{
   "feature": {
       "network": {
           "incoming": "mirror",
           "outgoing": true
       },
       "fs": "read",
       "env": true
   },
   "target": {
       "path": "deployment/nodejs-guestbook",
       "namespace": "default"
   }
}

```


If you want to mirror traffic from a multipod deployment, you can learn more about mirrord for teams /mirrord/docs/overview/teams/ which provides this feature. Right now, we only have one pod in this deployment, and mirrord’s OSS features should work perfectly for us.

To ensure that the configuration file created is read by the VSCode mirrord extension, hover over the mirrord button we mentioned earlier and press the ‘Select active config’ option. From the given prompt, enter the location of the configuration to be consumed by the plugin.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-01-35pm.png)

Once the active configuration is selected, we can start using mirrord with VSCode.

## Run and Debug with and without mirrord in VSCode

### Run and Debug with the mirrord extension disabled

Ensure that the mirrord extension is disabled.


![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-03-17pm.png)


Before running the application ensure that the dependencies have been satisfied by executing *npm install* in the root of the repository. After that, let’s Run and Debug our application without mirrord first by selecting Run and Debug from the Run and Debug tab.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-04-51pm.png)

Select the Node.js debugger from the drop-down menu.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-05-08pm.png)


The application fails to run with the below error because it doesn’t have access to the Redis instance running on Kubernetes.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-05-51pm.png)


Now let’s use mirrord to mirror the context from Kubernetes to our locally running Node.js application.

### Run and Debug with the mirrord plugin enabled

Enable the mirrord plugin by clicking on the mirrord button in the bottom left corner of the screen. The enabled button should look like the one below.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-07-05pm.png)


Once mirrord is enabled, let’s Run and Debug the application once again. Once we start the debugging, the dropdown will ask us which target to use for mirrord.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-07-33pm.png)


Upon choosing the Node.js Pod as the target, you can see logs similar to the ones below with the Node.js app starting successfully. 

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-08-00pm.png)


Once the app is running with the mirrord extension, access it on localhost:3000 in the browser. You will see the following UI for the mirrord guestbook application.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-08-41pm.png)


Now, let’s debug the application with mirrord.

## Debugging the application with the mirrord plugin

Now that we can run the application, let’s understand what our setup looks like with the mirrord-agent working with the target-impersonated Pod. The target impersonated Pod here is the Guestbook Pod.

![alt text](how-to-debug-a-nodejs-microservice/architecture-diagram-with-mirrord.png)


If you would like to learn more about how the mirrord-agent in the above architecture works, go check out the reference here /mirrord/docs/reference/architecture/#mirrord-agent.

We can now be sure that mirrord is working properly. 

Moving forward, let’s set a debug breakpoint in the application and see how it runs. I want to put a breakpoint in the application every time I create a note with the Guestbook application. The below line of code is where I am going to put the breakpoint.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-10-37pm.png)


Start debugging by pressing the debug button below.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-11-20pm.png)


You will be prompted to select a target pod for mirroring again. Choose the guestbook Pod. Once the debugging starts, you should be able to see the call stack and the terminal for debugging, like the one below.

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-11-36pm.png)


From http://localhost:3000, try publishing a new entry to the guestbook, which would hit the debugging entry point.

You should hit the breakpoint as shown below:

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-12-27pm.png)


We can debug the issue now as the breakpoint is hit.

You now know how to debug your Node.js microservice with VSCode + mirrord without having to build and deploy your application anew.

Next, let’s see how to debug our microservice in the CLI with the Node.js Debugger and mirrord.

## Debug in the CLI with Node.js and mirrord

## Run the application with npm in the CLI

Do a dry run of the application with the command below to see if the application starts without mirrord and the Kubernetes context we require for the Redis connection.

```bash 
npm install && npm start 
```


We get the below error after running the command above. 

![alt text](how-to-debug-a-nodejs-microservice/screenshot-2025-02-21-at-7-15-14pm.png)


The error states that the application is not able to connect to the Redis service. Let’s use mirrord to help our local application run with the Kubernetes context.

## Installing mirrord

Install the mirrord CLI tool and run Guestbook with the required Kubernetes context. Follow the installation guide for mirrord here /mirrord/docs/overview/quick-start/#installation.


## Run the guestbook application with Node.js and mirrord in the CLI

After installing, run the following command to start the Node.js guestbook application with mirrord:

```bash
mirrord exec -t deployment/nodejs-guestbook npm start 
```


You should see the following output, which will let you know that the Node.js Guestbook has started in debug mode.

```bash

⠒ mirrord exec
	✓ update to 3.126.0 available
	✓ ready to launch process
  	✓ layer extracted
  	✓ operator not found
  	✓ agent pod created
  	✓ pod is ready
  	✓ arm64 layer library extracted
	✓ config summary                                                                                  	 
> node-app@0.0.1 start
> node app.js

Guestbook app running on port 3000

```

After you have run the Guestbook program with mirrord you should be able to make your changes and rerun the service as necessary. You can even run the program in debug mode and attach a debugger if required.

## Debugging with mirrord vs. other debugging techniques

[mirrord](https://metalbear.com/mirrord/) distinguishes itself by eliminating the need for repeated building and deployment cycles. It allows developers to run the application locally while providing the necessary network and execution context of the target Kubernetes Pod. In this case, the local application behaves as if it were running within the cluster, enabling developers to debug using familiar tools without the overhead to build and deploy.

## Conclusion

This guide explored how to use mirrord in VSCode using the mirrord extension and the mirrord CLI. We demonstrated how developers can set breakpoints in their IDE or CLI debugger and step through code execution while leveraging the live Kubernetes environment.

By enabling local execution with Kubernetes context, mirrord helps developers save substantial time during debugging.

Curious to try it out? Give mirrord a go and see how it works for you. Got questions? [Hop into our Slack](https://metalbear.com/slack) and let us know!
