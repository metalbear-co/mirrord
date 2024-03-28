Remove `AgentManagment` because only `KubernetesAPI` implements it now and there is no need for this abstraction and moved the used functions straight onto `KubernetesAPI`.
