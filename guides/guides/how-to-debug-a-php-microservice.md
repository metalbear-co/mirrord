# How to Debug a PHP Microservice
In this guide, we’ll cover how to debug a PHP microservice running in a Kubernetes environment using mirrord. You’ll learn how to set up and use mirrord to debug PHP microservices in the command line.

Tip: You can use mirrord to debug, test, and troubleshoot your applications locally with Kubernetes context, without needing to build or deploy each time.

## Common debugging techniques for microservices in PHP
It can be cumbersome to debug microservices on Kubernetes. The lack of a debugging workflow for applications with multiple runtime dependencies in the context of Kubernetes makes it even harder. Why does it even make it harder? The following are common ways of debugging microservices with strict runtime environment dependencies:

### Continuous Deployment
Build a container image and deploy it to a Kubernetes cluster dedicated to testing or staging. The iterative process of building, deploying, and testing is resource-intensive and time-consuming, especially for testing frequent code changes.

### Log Analysis
One of the most common ways to understand the application behavior in a cluster is by analyzing logs. Adding extra logs to extract runtime information on the application is very common. Collecting and analyzing logs from different services can be effective but it isn’t the best real-time debugging solution.

### Remote Debugging
Developers can use remote debugging tools built into IDEs like PHPStorm to attach to processes already running in a Kubernetes cluster. While this allows real-time code inspection and interaction, it still requires heavy overhead from the IDE and a separate debug configuration for the deployment which can potentially affect the application's performance while debugging.

The above methods can be used by themselves or they can be used together.

## Challenges of debugging PHP microservices in Kubernetes
Debugging effectively within a Kubernetes context is the biggest challenge of working with Kubernetes. The build and release loop of the application can be short, but the process slows down development significantly. Nothing beats the ease and speed of debugging applications locally.

## Introduction to debugging PHP microservices with mirrord
With mirrord, we don’t have to think of building and releasing our applications for debugging. We can run our applications locally, and mirrord will make sure to have your locally running process in the context of Kubernetes. Context mirroring for processes allows your process to run locally and consume the resources of a remote resource. 

### Workload to process context mirroring
To achieve this, inputs from a Kubernetes workload (eg: a Pod) are mirrored to a locally running process. The process in question here today is a PHP process. Let’s see how we can mirror inputs for our locally running PHP application using mirrord and pipe these outputs back to Kubernetes. This will create a tighter feedback loop effectively allowing you to debug faster without the downsides of the common debugging techniques we discussed above.

### Sample application setup
In the example below, our PHP application will run locally. It will need to have the network information and environment of a Kubernetes Pod to debug. This Kubernetes Pod is running as part of a staging application deployment and will be our mirroring target.

Let’s get started with some prerequisites by setting up a test cluster and deploying our mirroring target.
### Prerequisites
Set up the Kubernetes cluster to test our application setup.

1. Start an instance of a development cluster like minikube, k3d, kind, etc. We are using minikube here.

```
minikube start
```

2. Clone the repo with the sample PHP application.

```
git clone https://github.com/waveywaves/mirrord-php-debug-example
cd mirrord-php-debug-example
```


3. Deploy our application which will act as our staging environment.

```
kubectl create -f ./kube

deployment.apps/php-guestbook created
service/php-guestbook created
deployment.apps/redis created
service/redis created
```


Once the above is deployed let’s use the following command to get access to the application endpoint so we can see what it looks like.

```
minikube service php-guestbook
```


The minikube service command automatically sets up a port forwarding session to the specified service and opens it in the default web browser. With the tunnel to our microservice setup, our application architecture now looks like this. 
![php application architecture](how-to-debug-a-php-microservice/screenshot-2025-03-24-at-12-28-34pm.png)



Once we run the minikube service command, we get this output. 

```

minikube service php-guestbook                                                        

|-----------|-----------|-------------|---------------------------|
| NAMESPACE |   NAME	| TARGET PORT |        	URL        	|
|-----------|-----------|-------------|---------------------------|
| default   | php-guestbook |    	4567 | http://192.168.49.2:31233 |
|-----------|-----------|-------------|---------------------------|
🏃  Starting tunnel for service guestbook.
|-----------|-----------|-------------|------------------------|
| NAMESPACE |   NAME	| TARGET PORT |      	URL       	|
|-----------|-----------|-------------|------------------------|
| default   | php-guestbook |         	| http://127.0.0.1:57485 |
|-----------|-----------|-------------|------------------------|
🎉  Opening service default/guestbook in default browser...
❗  Because you are using a Docker driver on darwin, the terminal needs to be open to run it.
```
![guestbook application webpage](how-to-debug-a-php-microservice/screenshot-2025-03-24-at-12-28-46pm.png)



Now we have access to the Guestbook application on http://localhost:57485.
Let’s access this URL from the browser.

![console output](how-to-debug-a-php-microservice/screenshot-2025-03-24-at-12-29-07pm.png)

We have our staging application deployed now. Let’s run the microservice with mirrord next. This will allow us to run the local PHP application in the context of Kubernetes without having to build and deploy it over and over again for testing. 
## Debug in the CLI with php and mirrord
1. Run the application with php in the CLI
Let’s run the following command to check if the application fails to run...

```
php -S localhost:8080 -t src/
```


On the run above we can see that the application run fails because this local execution doesn’t have access to the Redis instance running inside the Kubernetes cluster we have created. 

![guestbook application failure](how-to-debug-a-php-microservice/screenshot-2025-03-24-at-12-31-48pm.png)

The microservice needs access to the “redis” service hosted on the cluster. To run the PHP microservice with Kubernetes, we can use the mirrord CLI tool.

2. Installing mirrord

Install the mirrord CLI tool and run Guestbook with the required Kubernetes context. Follow the installation guide for mirrord [here](https://metalbear.com/mirrord/docs/overview/quick-start/#installation) .

3. Run the Guestbook application with php and mirrord in the CLI

Let’s install php-redis locally, as our code depends on it.

```
brew install php-redis
```


Then, run the following command to run the guestbook application in the context of Kubernetes.

```
mirrord exec -t deployment/php-guestbook -- php -S localhost:8080 -t src/ 
```


You should see the following output which will let you know that the guestbook has started in debug mode.


```
⠠ mirrord exec
	✓ update to 3.127.0 available
	✓ ready to launch process
  	✓ layer extracted
  	✓ operator not found
  	✓ agent pod created
  	✓ pod is ready
  	✓ arm64 layer library extracted
	✓ config summary    
    

[Wed Dec 11 22:58:39 2024] PHP 8.4.1 Development Server (http://localhost:8080) started
```   

The target impersonated Pod here is the Guestbook Pod. In this diagram, you can see how mirrord allows the user to use the mirrord-agent Pod as the execution context.

![application diagram with mirrord](how-to-debug-a-php-microservice/screenshot-2025-03-24-at-12-32-04pm.png)

After you have run the guestbook program with mirrord you should be able to make your changes and rerun the service as necessary. You can even run the program in debug mode and attach a debugger if required.

## Debugging with mirrord vs. other debugging techniques
[mirrord](https://metalbear.com/mirrord/) distinguishes itself by eliminating the need for repeated building and deployment cycles. It allows developers to run the application locally while providing the necessary network and execution context of the target Kubernetes Pod. In this case, the local application behaves as if it were running within the cluster, enabling developers to debug using familiar tools without the overhead to build and deploy.
## Conclusion

This guide explored how we can use mirrord in the command line for debugging PHP applications.
By enabling local execution with Kubernetes context, mirrord helps developers save substantial time during debugging.
Curious to try it out? Give [mirrord](https://metalbear.com/mirrord/) a go and see how it works for you. Got questions? Hop into our [Slack](https://metalbear.com/slack) and let us know!