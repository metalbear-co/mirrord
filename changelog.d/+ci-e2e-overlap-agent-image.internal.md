Run the CI e2e jobs concurrently with the agent image build instead of waiting for it, polling for the image artifact when it's actually needed, to keep the image build off the critical path.
