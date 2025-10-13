import ConfigTabs from "./ConfigTabs";

const ConfigStep = () => {
    return {
    id: "config-new-user",
    title: "Configuration Setup",
    content: (
      <div className="space-y-4">
        <div className="text-center space-y-4">
          <p className="text-sm text-muted-foreground">
            Configure your mirrord settings using the tabs below
          </p>
          <ConfigTabs />
        </div>
      </div>
    )
  };
};

export default ConfigStep