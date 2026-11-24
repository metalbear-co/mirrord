# How to Debug a Kotlin Microservice
In this guide, we’ll cover how to debug a Kotlin microservice running in a Kubernetes environment using mirrord. You’ll learn how to set up and use mirrord with the IntelliJ IDEA IDE and use the mirrord CLI to debug the Kotlin application in the command line.

**Tip:** You can use mirrord to debug, test, and troubleshoot your applications locally with Kubernetes context, without needing to build or deploy each time.

## Common debugging techniques for microservices in Kotlin
It can be cumbersome to debug microservices on Kubernetes. The lack of a debugging workflow for applications with multiple runtime dependencies in the context of Kubernetes makes it even harder. Why does it even make it harder? The following are common ways of debugging microservices with strict runtime environment dependencies:

### Continuous Deployment
Build a container image and deploy it to a Kubernetes cluster dedicated to testing or staging. The iterative process of building, deploying, and testing is resource-intensive and time-consuming, especially for testing frequent code changes.

### Log Analysis
One of the most common ways to understand the application behavior in a cluster is by analyzing logs. Adding extra logs to extract runtime information on the application is very common. Collecting and analyzing logs from different services can be effective but it isn’t the best real-time debugging solution.
### Remote Debugging
Developers can use remote debugging tools built into IDEs like IntelliJ IDEA to attach to processes already running in a Kubernetes cluster. While this allows real-time code inspection and interaction, it still requires heavy overhead from the IDE and a separate debug configuration for the deployment which can potentially affect the application's performance while debugging.

The above methods can be used by themselves or they can be used together.
## Challenges of debugging Kotlin microservices in Kubernetes
Debugging effectively within a Kubernetes context is the biggest challenge of working with Kubernetes. The build and release loop of the application can be short, but the process slows down development significantly. Nothing beats the ease and speed of debugging applications locally.
## Introduction to debugging Kotlin microservices with mirrord
With mirrord, we don’t have to think of building and releasing our applications for debugging. We can run our applications locally and mirrord will make sure to have your locally running process in the context of Kubernetes. Context mirroring for processes allows your process to run locally and consume the resources of a remote resource. 
#### Workload to process context mirroring
To achieve this, inputs from a Kubernetes workload (eg: a Pod) are mirrored to a locally running process. The process in question here today is a Kotlin process. Let’s see how we can mirror inputs for our locally running Kotlin application using mirrord and pipe these outputs back to Kubernetes. This will create a tighter feedback loop effectively allowing you to debug faster without the downsides of the common debugging techniques we discussed above.
#### Sample application setup
In the example below, our Kotlin application will run locally. It will need to have the network information and environment of a Kubernetes Pod to debug. This Kubernetes Pod is running as part of a staging application deployment and will be our mirroring target. 
### Prerequisites
Set up the Kubernetes cluster to test our application setup.

1. Start an instance of a development cluster like minikube, k3d, kind, etc. We are using minikube here.

```
minikube start
```

2. Clone the repo with the sample Kotlin application.

```
git clone https://github.com/waveywaves/mirrord-kotlin-debug-example
cd mirrord-kotlin-debug-example
```

3. Deploy our application which will act as our staging environment.

```
kubectl create -f ./kube

deployment.apps/kotlin-guestbook created
deployment.apps/redis created
service/redis created
service/kotlin-guestbook created
```


Once the above is deployed let’s use the following command to get access to the application endpoint so we can see what it looks like.

```
minikube service kotlin-guestbook
```


The above minikube service command automatically sets up a port forwarding session to the specified service and opens it in the default web browser. With the tunnel to our microservice setup, our application architecture now looks like this. 

![kotlin application architecture](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-07-40pm.png)

Once we run the minikube service command, we get this output. 

```
minikube service kotlin-guestbook

|-----------|-------|-------------|---------------------------|
| NAMESPACE | NAME  | TARGET PORT |        	URL        	|
|-----------|-------|-------------|---------------------------|
| default   | kotlin-guestbook |      	80 | http://192.168.49.2:30742 |
|-----------|-------|-------------|---------------------------|
🏃  Starting tunnel for service kotlin-guestbook.
|-----------|-------|-------------|------------------------|
| NAMESPACE | NAME  | TARGET PORT |      	URL       	|
|-----------|-------|-------------|------------------------|
| default   | kotlin-guestbook |         	| http://127.0.0.1:52030 |
|-----------|-------|-------------|------------------------|
🎉  Opening service default/kotlin-guestbook in default browser...
❗  Because you are using a Docker driver on darwin, the terminal needs to be open to run it.
```


Now we have access to the note-taking application on http://localhost:52030.
Let’s access this URL from the browser.



We have our staging application deployed now. Let’s run the microservice with mirrord now. This will allow us to run the local Kotlin application in the context of Kubernetes without having to build and deploy it over and over again for testing.

## Debug the Guestbook application with IntelliJ IDEA and the JetBrains mirrord plugin
In this section of the guide, we are going to use the mirrord Intellij Plugin to help debug the Kotlin application. If you would like to see how we can do the same with the Kotlin Debugger in the CLI, go to [this section of the guide](https://metalbear.co/guides/how-to-debug-a-kotlin-microservice/#:~:text=Debugger%20and%20mirrord.-,Debug%20in%20the%20CLI%20with%20mvn%20and%20mirrord,-1.%20Run%20the).

The application in question is Guestbook, a simple note-taking app written in Kotlin with support for storing notes in Redis. The source code for the test application is available on GitHub at https://github.com/waveywaves/mirrord-kotlin-debug-example. We will use it as a follow-along Kotlin application for debugging with mirrord.
### 1. Setup IntelliJ IDEA with the mirrord plugin
To get started, install mirrord in IntelliJ IDEA.
#### Installing the mirrord plugin

You can install the plugin by searching for the Plugin in the Plugins settings. 

![screenshot of mirrord plugin](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-07-03pm.png)

After installing the plugin and restarting the IDE, a dialog box like the one below will appear.

![screenshot of mirrord icon](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-06-35pm.png)

You will see a mirrord button and a dropdown menu in the top right corner of the screen.

This dialog box appears as mirrord is successfully installed. It contains the directions for how to use mirrord in the IntelliJ IDEA IDE. Let’s set up the mirrord configuration.

![mirrord configurations](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-06-17pm.png)

#### mirrord configuration
You will see a mirrord button and a dropdown menu in the top right corner of the screen.


![setting up mirrord](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-06-06pm.png)

If you don’t already have a .mirrord/.mirrord.json configuration file for this application, you can create one using the Settings option in the plugin dropdown menu.

![creating mirrord config file](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-05-54pm.png)

As a new config file is opened up in your editor, you can update the contents of the file to the ones below so that you choose the kotlin-guestbook pod deployed to the default namespace as your mirrord target. This will ensure that your mirrord debug process will run in the context of the kotlin-guestbook deployment.

```

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
       "path": "deployment/kotlin-guestbook",
       "namespace": "default"
   }
}
```

If you want to mirror traffic from a multipod deployment, you can learn more about [mirrord for teams]( /mirrord/docs/overview/teams/) which provides this feature. Right now we only have one pod in this deployment and mirrord’s OSS features should work perfectly for us.

### 2. How to run the application with and without mirrord in IntelliJ IDEA?

#### Running the application with the mirrord plugin disabled

Before starting, make sure that you have updated your run configuration in IntelliJ IDEA with the following command to execute this Kotlin app with Maven.

```
mvn compile exec:java
```

A disabled mirrord button will be shown with a slash in front of it if mirrord is disabled.

![screenshot of mirrord disabled](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-05-34pm.png)

After running the above command you should see that the application fails to start because it’s not able to connect to the “redis” service.

![unable to connect to redis](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-05-19pm.png)

#### Running the application with the mirrord plugin enabled
Click the “mirrord” button to enable it. You’ll see a “mirrord enabled” notification in the bottom-right corner of your screen.

![screenshot of mirrord enabled](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-05-02pm.png)
Once mirrord is enabled, let’s run the application once again. 
Below you can see the successful run of the kotlin-guestbook application. 

![screnshot of kotlin application](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-04-45pm.png)
As the application starts successfully, you should be able to access the Kotlin application listening on http://localhost:8080 on your local machine. Let’s access the endpoint in the browser.

![accessing endpoint in browser](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-04-20pm.png)

We can see that this debuggable instance of Guestbook also has access to the stored notes in Redis which are reflected in the Notes section of the application. 

### 3. Debugging the application with the mirrord plugin
Now that we can run the application, let’s understand what our setup looks like with the mirrord-agent working with the target-impersonated Pod. The target impersonated Pod here is the Guestbook Pod.

![debugging apps in mirrord](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-04-09pm.png)


If you would like to learn more about how the mirrord-agent in the above architecture works, go check out the reference [here](https://metalbear.com/mirrord/docs/reference/architecture/#mirrord-agent).

We can now be sure that mirrord is working properly. 

Moving forward, let’s set a debug breakpoint in the application and see how it runs. I want to put a breakpoint in the application every time I create a note with the Guestbook application. The below line of code is where I am going to put the breakpoint.

![screenshot setting a debug breakpoint](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-03-53pm.png)

Start debugging by pressing the debug button below.

![screenshot of starting debugging in mirrord](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-03-35pm.png)

From http://localhost:8080 create a new note and publish it. 
On the application run, you should hit the breakpoint as shown below:

![hitting breakpoint in running app](how-to-debug-a-kotlin-microservice/screenshot-2025-03-14-at-4-03-04pm.png)

We can debug the issue now as the breakpoint is hit. 

You now know how to debug your Kotlin microservice with IntelliJ IDEA + mirrord without having to build and deploy your application anew. 

Next, let’s see how we can debug our microservice in the CLI with the Kotlin Debugger and mirrord.

## Debug in the CLI with mvn and mirrord
### 1. Run the application with Maven in the CLI
Let’s run the following command to run the application with Maven locally.

```
mvn compile exec:java
```

On the run above we can see that the application run fails because this local execution doesn’t have access to the services running inside the Kubernetes cluster we have created. 

The microservice needs access to the “redis” service hosted on the cluster. To run the microservice with Kubernetes, we can use the mirrord CLI tool.
### 2. Install mirrord
Let’s install the mirrord CLI tool and run kotlin-guestbook with the required Kubernetes context. Follow the installation guide for mirrord [here](https://metalbear.com/mirrord/docs/overview/quick-start/#installation) and run the below command.

### 3. Run the application with Maven and mirrord in the CLI
```
mirrord exec -t deployment/kotlin-guestbook -- mvn compile exec:java
```


The above command runs the kotlin-guestbook application in the context of the “kotlin-guestbook” Deployment.
With that, we know how to use mirrord to debug a Kotlin application with a Kubernetes context without the hassle of deploying or using an IDE for debugging.

## Debugging with mirrord vs. other debugging techniques
mirrord distinguishes itself by eliminating the need for repeated building and deployment cycles. It allows developers to run the application locally while providing it with the necessary network and execution context of the target Kubernetes Pod. In this case, the local application behaves as if it were running within the cluster, enabling developers to debug using familiar tools without the overhead to build and deploy.

## Conclusion
In this guide, we explored how to use mirrord with IntelliJ IDEA through a plugin, as well as with the mirrord CLI. We demonstrated how developers can set breakpoints in their IDE or CLI debugger and step through code execution while leveraging the live Kubernetes environment.

By enabling local execution with Kubernetes context, mirrord helps developers save substantial time during debugging.

Curious to try it out? Give [mirrord](https://metalbear.com/mirrord/) a go and see how it works for you. Got questions? [Hop into our Slack](https://metalbear.com/slack) and let us know!
