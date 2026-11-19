# How to Debug a Java Microservice

In this guide, we’ll cover how to debug a Java microservice running in a Kubernetes environment using mirrord. You’ll learn how to set up and use mirrord with IntelliJ IDEA and the command-line Java Debugger (jdb).

---

**_Tip:_** You can use [mirrord](https://metalbear.com/mirrord/) to debug, test, and troubleshoot your applications locally with Kubernetes context, without needing to build or deploy each time.

---

## Common debugging techniques for microservices in Java

Debugging microservices in Kubernetes can be quite cumbersome, especially due to the lack of a structured debugging workflow for applications with runtime dependencies. This complexity makes the debugging process even more challenging. Let’s explore a few common methods for debugging microservices that have strict runtime environment dependencies: 

### Build and deploy to a test environment

Build a container image and deploy it to a Kubernetes cluster dedicated for testing or staging. The iterative process of building, deploying, and testing is resource-intensive and time-consuming, especially for testing frequent code changes.

### Log analysis

One of the most common ways to understand the application behaviour in a cluster is analyzing logs. Adding extra logs to extract runtime information on the application is very common. Collecting and analyzing logs from different services can be effective but it isn’t the best real time debugging solution.

### Remote debugging

Developers can use remote debugging tools built into IDEs like IntelliJ IDEA to attach to processes already running in a Kubernetes cluster. While this allows real-time code inspection and interaction, it still requires heavy overhead from the IDE and a separate debug configuration for the deployment which can potentially affect the application's performance while debugging.

## Challenges of debugging Java microservices in Kubernetes

As someone who develops applications for Kubernetes, one of the biggest challenges I face is debugging them effectively within a Kubernetes context. Even if the build and release loop of my application is short, the process slows down development compared to the ease and speed of debugging applications locally.

## Introduction to debugging Java microservices with mirrord

With mirrord, we don’t have to think of building and releasing our applications now. We can run our applications locally and mirrord will ensure your locally running process thinks that it is running in the context of Kubernetes. The application runs locally, but it will have the network and execution context of your preferred Kubernetes pod as a target. Traffic from your Kubernetes context is mirrored (or stolen if you prefer that) to the application with which you can test your application against the services running in your cluster without having to deploy the process on your cluster.

### Prerequisites

Let’s set up the Kubernetes cluster we would like to test our application in and let’s clone the repository which houses this application so we can develop and test it locally.

1. Let’s start our minikube cluster for this demonstration. Run the command below to create the minikube cluster.

```bash
minikube start
```

I am using minikube here as my test Kubernetes cluster. You can use k3d or kind instead of minikube (or even your staging cluster). 

2. Clone the repo which contains all the resources necessary to develop our application.

```bash
git clone https://github.com/waveywaves/mirrord-java-debug-example cd mirrord-java-debug-example
```

3. Deploy our application onto the cluster to see what it looks like. Also, create a namespace to run the deployment and its dependencies (Services, PersistentVolumeClaims).

```console
kubectl create -f ./kube

deployment.apps/knote created
service/knote created
persistentvolumeclaim/minio-pvc created
service/minio created
deployment.apps/minio created
persistentvolumeclaim/mongo-pvc created
service/mongo created
```

Once the above is deployed let’s use the following command to get access to the application endpoint so we can see what it looks like.

### Running the SpringBoot Microservice without mirrord

```md
minikube service knote

|-----------|-------|-------------|---------------------------|
| NAMESPACE | NAME  | TARGET PORT |        	URL        	|
|-----------|-------|-------------|---------------------------|
| default   | knote |      	80 | http://192.168.49.2:30742 |
|-----------|-------|-------------|---------------------------|
🏃  Starting tunnel for service knote.
|-----------|-------|-------------|------------------------|
| NAMESPACE | NAME  | TARGET PORT |      	URL       	|
|-----------|-------|-------------|------------------------|
| default   | knote |         	| http://127.0.0.1:52030 |
|-----------|-------|-------------|------------------------|
🎉  Opening service default/knote in default browser...
❗  Because you are using a Docker driver on darwin, the terminal needs to be open to run it.
```

Once the above command runs, we will have access to the knote application on [http://localhost:52030](http://localhost:52030) as you can see above.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-00-06-1.png)

The above minikube service command automatically sets up a port forwarding session to the specified service and opens it in the default web browser. With the port-forward to our microservice setup, our application architecture now looks like this:

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-01-10-1.png)

Now that we have our application running in Kubernetes, let’s run the microservice with mirrord without having to deploy it on the Kubernetes cluster.

## Using Intellij IDEA + mirrord (plugin) to debug a SpringBoot application

In this section of the guide, we are going to use the mirrord Intellij Plugin to help debug the Java application. If you would like to see how we can do the same with the Java Debugger in the CLI, go to this section of the guide. 

The application in question is knote, a simple note taking app written in SpringBoot with support for storing notes in MongoDB and attachments in MinIO. The source code for the test application is available on github at [https://github.com/waveywaves/mirrord-java-debug-example](https://github.com/waveywaves/mirrord-java-debug-example). We are going to use it as a follow along Java application for debugging with mirrord.

1. **Setting up IntelliJ IDEA + mirrord**

#### Setup

To get started, install mirrord in IntelliJ IDEA.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-04-02-1.png)

After installing the plugin and restarting the IDE, a dialog box like the one below will appear.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-04-48-1.png)

You will see a mirrord button and a dropdown menu in the top right corner of the screen.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-05-00-1.png)

#### mirrord configuration

If you don’t already have a .mirrord/.mirrord.json configuration file for this application, you can create one using the Settings option in the plugin dropdown menu.

![alt text](how-to-debug-a-java-microservice/group-3473.png)

As a new config file is opened up in your editor, you can update the contents of the file to the ones below so that you choose the knote pod deployed to the default namespace as your mirrord target. This will ensure that your mirrord debug process will run in the context of the knote deployment.

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
       "path": "deployment/knote",
       "namespace": "default"
   }
}
```

If you want to mirror traffic from a multipod deployment, you can learn more about mirrord for teams [/mirrord/docs/overview/teams/](https://metalbear.com/mirrord/docs/overview/teams/) which provides this feature. Right now we only have one pod in this deployment and mirrord’s OSS features should work perfectly for us.

#### Testrun without mirrord enabled

![alt text](how-to-debug-a-java-microservice/group-3474.png)

Choose the config we just updated as the active configuration by selecting the button above and setting it as the active config. Once that’s done, start the application with mirrord and see if it works in the context of the cluster. We should be able to create notes in the application.

To demonstrate that the knote fails when running in the context of its dependencies, we are going to build and run the application locally from the IDE by pressing the “play” button in the top right corner of the IDE which is visible below. The dependencies in question here are the Kubernetes Pod where knote is deployed.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-17-57-1.png)

After running the above command you should see that the application fails to start because it’s not able to connect to the “minio” or “mongo” Services.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-02-at-20-18-51-1.png)

2. **Execution with the mirrord plugin**

Click the “mirrord” button to enable it. You’ll see a “mirrord enabled” notification in the bottom-right corner of your screen.

![alt text](how-to-debug-a-java-microservice/group-3498.png)

Once mirrord is enabled, let’s run the application once again.  
Below you can see the successful run of the knote application.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-40-15-1.png)

As the application starts successfully, you should be able to access the java application listening on the [http://localhost:8080](http://localhost:8080) on your local machine. Let’s access the endpoint in the browser.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-42-08-1.png)

We can see that this debuggable instance of knote also has access to the stored notes in the mongodb database which is reflected under the Notes section of the application. The application has been exposed on 8080 because Springboot sees that port 80 on the Pod is already bound and the next best port (8080) has been picked by Springboot to expose this application. If this application were running locally with all dependencies satisfied and nothing listening on port 80, it would be exposed on port 80 itself.

3. **Debugging with the mirrord plugin**

Now that we are able to run the application, let’s understand what our setup looks like with the mirrord-agent working with the target impersonated Pod. The target impersonated Pod here is the knote Pod.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-43-53-1.png)

If you would like to learn more about how the mirrord-agent in the above architecture works, go checkout the reference here [/mirrord/docs/reference/architecture/#mirrord-agent](https://metalbear.com/mirrord/docs/reference/architecture/#mirrord-agent).

We can now be sure that mirrord is working properly.

Moving forward, let’s set a debug breakpoint in the application and see how it runs. I want to put a breakpoint in the application every time I create a note with the knote application. The below line of code is where I am going to put the breakpoint.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-44-44-1.png)

Start debugging by pressing the debug button below.

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-46-47-1.png)

From [http://localhost:8080](http://localhost:8080) create a new note and publish it.  
On the application run, you should hit the breakpoint as shown below:

![alt text](how-to-debug-a-java-microservice/screenshot-2024-12-03-at-08-47-16-1.png)

We can debug the issue now as the breakpoint is hit.

You now know how to debug your Java microservice with IntelliJ IDEA + mirrord without having to build and deploy your application anew.

Next, let’s see how we can debug our microservice in the CLI with the Java Debugger and mirrord.

## Using the Java debugger (jdb) + mirrord to debug in the CLI

1. jdb + mirrord

You should already have jdb installed locally if you have the Java Development Kit of your choice installed on your system. To work effectively with jdb, let’s make sure that the application runs properly first. Let’s run the following command to check the same (we expect this execution to fail).

```bash
mvn spring-boot:run
```

On the run above we can see that the application run fails because this local execution doesn’t have access to the services running inside the Kubernetes cluster we have created. 

The microservice needs access to the “mongo” and “minio” services hosted on the cluster. To run the SpringBoot microservice with the Kubernetes, we can use mirrord CLI tool.

#### Installing mirrord

Let’s install mirrord CLI tool and run knote with the required Kubernetes context. Follow the installation guide for mirrord here [/mirrord/docs/overview/quick-start/#installation](https://metalbear.com/mirrord/docs/overview/quick-start/#installation) and run the below command.

#### Running the SpringBoot application with mirrord

```bash
mirrord exec -t deployment/knote mvn spring-boot:run
```

The above command runs the knote SpringBoot application in the context of the “knote” Deployment.

2. **Run the microservice in debug mode**

As the above command runs we can debug this application using mvnDebug. Run the following command to start knote in debug mode.

```bash
mirrord exec -t deployment/knote mvnDebug spring-boot:run
```

You should see the following output which will let you know that the knote has started in debug mode.

```console
...
Preparing to execute Maven in debug mode
OpenJDK 64-Bit Server VM warning: Option -Xdebug was deprecated in JDK 22 and will likely be removed in a future release.
Listening for transport dt_socket at address: 8000
```

3. **Debugging with the Java debugger**

In another terminal instance let’s run the following command and attach our debugger to the maven process. 

```bash
jdb -sourcepath ./src/main/java -attach 8000
```

Even if the Java application has the context of the kubernetes cluster, its ports are still opened on the local machine. So we can attach our debugger to the 8000 port.

#### Using jdb for debugging

The above command will ensure that we attach to the debuggable knote process. You will see the following output if the debugger successfully attaches to knote.

```console
Set uncaught java.lang.Throwable
Set deferred uncaught java.lang.Throwable
Initializing jdb ...

VM Started: > No frames on the current call stack

main[1]
```

Set a breakpoint and debug your process with the following command in debugger.

```bash
main [1] stop at KnoteJavaApplication:142
```

You can use the ‘step’ and/or ‘next’ commands to continue the execution of the process.

The location of the breakpoint requires us to use the application to publish a note. Go to [http://localhost:8080](http://localhost:8080) and create a note which will call the /note POST endpoint executing through this particular line of code where the breakpoint is set.

Once you hit the breakpoint you will be able to use jdb to dig further into the execution context of the application.

With that, we know how to use mirrord and jdb together to debug a Java application with a Kubernetes context without the hassle of deploying or using an IDE for debugging.

## Debugging with mirrord vs. other debugging techniques

mirrord distinguishes itself by eliminating the need for repeated building and deployment cycles. It allows developers to run the application locally while providing it with the necessary network and execution context of the target Kubernetes Pod. In this case the local application behaves as if it were running within the cluster, enabling developers to debug using familiar tools without the overhead to build and deploy.

## Conclusion

In this guide, we explored how to use mirrord with IntelliJ IDEA through a plugin, as well as with the command-line Java Debugger (jdb). We demonstrated how developers can set breakpoints in their IDE or CLI debugger and step through code execution while leveraging the live Kubernetes environment.

By enabling local execution with Kubernetes context, mirrord helps developers save substantial time during debugging.

Curious to try it out? [Give mirrord a go](https://app.metalbear.com/account/sign-up) and see how it works for you. Got questions? [Hop into our Slack](https://metalbear.com/slack) and let us know!