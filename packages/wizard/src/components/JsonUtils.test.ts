import { describe, it, expect } from "vitest";
import {
  readBoilerplateType,
  updateConfigMode,
  updateConfigCopyTarget,
  readCurrentTargetDetails,
  updateConfigTarget,
  readCurrentFilters,
  updateConfigFilter,
  disableConfigFilter,
  readCurrentPorts,
  updateConfigPorts,
  readCurrentPortMapping,
  addRemoveOrUpdateMapping,
  removePortandMapping,
  getLocalPort,
  getConfigString,
  regexificationRay,
} from "./JsonUtils";
import type { LayerFileConfig } from "../mirrord-schema";

describe("JsonUtils", () => {
  describe("readBoilerplateType", () => {
    it('returns "mirror" for mirror mode config', () => {
      const config: LayerFileConfig = {
        feature: {
          network: {
            incoming: { mode: "mirror" },
          },
        },
      };
      expect(readBoilerplateType(config)).toBe("mirror");
    });

    it('returns "steal" for steal mode with copy_target disabled', () => {
      const config: LayerFileConfig = {
        feature: {
          copy_target: { enabled: false, scale_down: false },
          network: {
            incoming: { mode: "steal" },
          },
        },
      };
      expect(readBoilerplateType(config)).toBe("steal");
    });

    it('returns "replace" for steal mode with copy_target enabled and scale_down', () => {
      const config: LayerFileConfig = {
        feature: {
          copy_target: { enabled: true, scale_down: true },
          network: {
            incoming: { mode: "steal" },
          },
        },
      };
      expect(readBoilerplateType(config)).toBe("replace");
    });

    it('returns "custom" for empty config', () => {
      const config: LayerFileConfig = {};
      expect(readBoilerplateType(config)).toBe("custom");
    });

    it('returns "custom" for steal mode without copy_target', () => {
      const config: LayerFileConfig = {
        feature: {
          network: {
            incoming: { mode: "steal" },
          },
        },
      };
      expect(readBoilerplateType(config)).toBe("custom");
    });
  });

  describe("updateConfigMode", () => {
    it("sets mirror mode correctly", () => {
      const config: LayerFileConfig = {};
      const result = updateConfigMode("mirror", config);
      expect(result.feature?.network?.incoming).toEqual({ mode: "mirror" });
    });

    it("sets steal mode correctly", () => {
      const config: LayerFileConfig = {};
      const result = updateConfigMode("steal", config);
      expect(result.feature?.network?.incoming).toEqual({ mode: "steal" });
    });

    it("preserves existing config properties", () => {
      const config: LayerFileConfig = {
        target: { path: "deployment/test" },
      };
      const result = updateConfigMode("mirror", config);
      // Use type guard to check target has path property
      const target = result.target;
      expect(target).toBeDefined();
      expect(
        typeof target === "object" && target !== null && "path" in target
          ? target.path
          : undefined,
      ).toBe("deployment/test");
    });
  });

  describe("updateConfigCopyTarget", () => {
    it("enables copy_target with scale_down", () => {
      const config: LayerFileConfig = {};
      const result = updateConfigCopyTarget(true, true, config);
      expect(result.feature?.copy_target).toEqual({
        enabled: true,
        scale_down: true,
      });
    });

    it("sets copy_target to disabled", () => {
      const config: LayerFileConfig = {
        feature: {
          copy_target: { enabled: true, scale_down: true },
        },
      };
      const result = updateConfigCopyTarget(false, false, config);
      expect(result.feature?.copy_target).toEqual({
        enabled: false,
        scale_down: false,
      });
    });
  });

  describe("readCurrentTargetDetails", () => {
    it("parses deployment target from path", () => {
      const config: LayerFileConfig = {
        target: {
          path: "deployment/my-app",
          namespace: "production",
        },
      };
      const result = readCurrentTargetDetails(config);
      expect(result).toEqual({
        type: "deployment",
        name: "my-app",
      });
    });

    it("parses pod target from path", () => {
      const config: LayerFileConfig = {
        target: {
          path: "pod/my-pod",
          namespace: "default",
        },
      };
      const result = readCurrentTargetDetails(config);
      expect(result).toEqual({
        type: "pod",
        name: "my-pod",
      });
    });

    it("returns targetless for missing target", () => {
      const config: LayerFileConfig = {};
      const result = readCurrentTargetDetails(config);
      expect(result).toEqual({
        type: "targetless",
      });
    });

    it("parses deployment object format", () => {
      const config: LayerFileConfig = {
        target: {
          deployment: "my-deployment",
        },
      };
      const result = readCurrentTargetDetails(config);
      expect(result).toEqual({
        type: "deployment",
        name: "my-deployment",
      });
    });

    it("parses string target format", () => {
      const config: LayerFileConfig = {
        target: "deployment/my-app",
      };
      const result = readCurrentTargetDetails(config);
      expect(result).toEqual({
        type: "deployment",
        name: "my-app",
      });
    });
  });

  describe("updateConfigTarget", () => {
    it("sets target path and namespace", () => {
      const config: LayerFileConfig = {};
      const result = updateConfigTarget(config, "deployment/api", "staging");
      expect(result.target).toEqual({
        path: "deployment/api",
        namespace: "staging",
      });
    });
  });

  describe("readCurrentFilters", () => {
    it("reads single header filter", () => {
      const config = {
        feature: {
          network: {
            incoming: {
              mode: "steal" as const,
              http_filter: {
                header: "x-user-id: 123",
              },
            },
          },
        },
      } as LayerFileConfig;
      const result = readCurrentFilters(config);
      expect(result.filters).toHaveLength(1);
      expect(result.filters).toContainEqual({
        type: "header",
        value: "x-user-id: 123",
      });
    });

    it("reads single path filter", () => {
      const config = {
        feature: {
          network: {
            incoming: {
              mode: "steal" as const,
              http_filter: {
                path: "/api/users",
              },
            },
          },
        },
      } as LayerFileConfig;
      const result = readCurrentFilters(config);
      expect(result.filters).toHaveLength(1);
      expect(result.filters).toContainEqual({
        type: "path",
        value: "/api/users",
      });
    });

    it("reads any_of filters", () => {
      const config = {
        feature: {
          network: {
            incoming: {
              mode: "steal" as const,
              http_filter: {
                any_of: [{ header: "x-user-id: 123" }, { path: "/api/users" }],
              },
            },
          },
        },
      } as LayerFileConfig;
      const result = readCurrentFilters(config);
      expect(result.filters).toHaveLength(2);
      expect(result.operator).toBe("any");
      expect(result.filters).toContainEqual({
        type: "header",
        value: "x-user-id: 123",
      });
      expect(result.filters).toContainEqual({
        type: "path",
        value: "/api/users",
      });
    });

    it("returns empty filters for no http_filter", () => {
      const config: LayerFileConfig = {};
      const result = readCurrentFilters(config);
      expect(result.filters).toHaveLength(0);
    });
  });

  describe("updateConfigFilter", () => {
    it("adds header filter", () => {
      const config: LayerFileConfig = {
        feature: {
          network: {
            incoming: { mode: "steal" },
          },
        },
      };
      const filters = [{ type: "header" as const, value: "x-test: value" }];
      const result = updateConfigFilter(filters, "any", config);
      const incoming = result.feature?.network?.incoming;
      expect(
        typeof incoming === "object" &&
          incoming !== null &&
          "http_filter" in incoming,
      ).toBe(true);
    });

    it("adds path filter", () => {
      const config: LayerFileConfig = {
        feature: {
          network: {
            incoming: { mode: "steal" },
          },
        },
      };
      const filters = [{ type: "path" as const, value: "/api/test" }];
      const result = updateConfigFilter(filters, "any", config);
      const incoming = result.feature?.network?.incoming;
      expect(
        typeof incoming === "object" &&
          incoming !== null &&
          "http_filter" in incoming,
      ).toBe(true);
    });
  });

  describe("disableConfigFilter", () => {
    it("removes http_filter from config", () => {
      const config = {
        feature: {
          network: {
            incoming: {
              mode: "steal" as const,
              http_filter: {
                header: "x-test: value",
              },
            },
          },
        },
      } as LayerFileConfig;
      const result = disableConfigFilter(config);
      const network = result.feature?.network;
      const incoming =
        typeof network === "object" && network !== null && "incoming" in network
          ? network.incoming
          : undefined;
      expect(
        typeof incoming === "object" &&
          incoming !== null &&
          "http_filter" in incoming,
      ).toBe(false);
    });
  });

  describe("Port management", () => {
    describe("readCurrentPorts", () => {
      it("reads ports from incoming.ports config", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
                ports: [8080, 3000],
              },
            },
          },
        };
        const result = readCurrentPorts(config);
        expect(result).toEqual([8080, 3000]);
      });

      it("returns empty array for no ports", () => {
        const config: LayerFileConfig = {};
        const result = readCurrentPorts(config);
        expect(result).toEqual([]);
      });
    });

    describe("updateConfigPorts", () => {
      it("adds ports to config", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: { mode: "steal" },
            },
          },
        };
        const result = updateConfigPorts([8080, 3000], config);
        const incoming = result.feature?.network?.incoming;
        expect(
          typeof incoming === "object" &&
            incoming !== null &&
            "ports" in incoming,
        ).toBe(true);
        if (
          typeof incoming === "object" &&
          incoming !== null &&
          "ports" in incoming
        ) {
          expect(incoming.ports).toEqual([8080, 3000]);
        }
      });
    });

    describe("readCurrentPortMapping", () => {
      it("reads port mappings from port_mapping", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
                port_mapping: [
                  [9000, 8080],
                  [4000, 3000],
                ],
              },
            },
          },
        };
        const result = readCurrentPortMapping(config);
        expect(result).toContainEqual([9000, 8080]);
        expect(result).toContainEqual([4000, 3000]);
      });

      it("returns empty array for no port_mapping", () => {
        const config: LayerFileConfig = {};
        const result = readCurrentPortMapping(config);
        expect(result).toEqual([]);
      });
    });

    describe("addRemoveOrUpdateMapping", () => {
      it("adds new port mapping", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
              },
            },
          },
        };
        const result = addRemoveOrUpdateMapping(8080, 9000, config);
        const mappings = readCurrentPortMapping(result);
        expect(mappings).toContainEqual([9000, 8080]);
      });

      it("removes mapping when local equals remote", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
                port_mapping: [[9000, 8080]],
              },
            },
          },
        };
        const result = addRemoveOrUpdateMapping(8080, 8080, config);
        const mappings = readCurrentPortMapping(result);
        expect(mappings).not.toContainEqual([9000, 8080]);
      });
    });

    describe("removePortandMapping", () => {
      it("removes port and its mapping", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
                ports: [8080, 3000],
                port_mapping: [[9000, 8080]],
              },
            },
          },
        };
        const result = removePortandMapping(8080, config);
        const ports = readCurrentPorts(result);
        expect(ports).not.toContain(8080);
        expect(ports).toContain(3000);
      });
    });

    describe("getLocalPort", () => {
      it("returns mapped local port", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
                port_mapping: [[9000, 8080]],
              },
            },
          },
        };
        expect(getLocalPort(8080, config)).toBe(9000);
      });

      it("returns same port when not mapped", () => {
        const config: LayerFileConfig = {
          feature: {
            network: {
              incoming: {
                mode: "steal",
              },
            },
          },
        };
        expect(getLocalPort(8080, config)).toBe(8080);
      });
    });
  });

  describe("getConfigString", () => {
    it("returns formatted JSON string", () => {
      const config: LayerFileConfig = {
        target: { path: "deployment/test" },
      };
      const result = getConfigString(config);
      expect(result).toContain('"target"');
      expect(result).toContain('"path"');
      expect(result).toContain("deployment/test");
    });

    it("returns valid JSON", () => {
      const config: LayerFileConfig = {
        target: { path: "deployment/test", namespace: "default" },
        feature: {
          network: {
            incoming: { mode: "mirror" },
          },
        },
      };
      const result = getConfigString(config);
      expect(() => JSON.parse(result)).not.toThrow();
    });
  });

  describe("regexificationRay", () => {
    it("wraps simple string in regex anchors", () => {
      const result = regexificationRay("test");
      expect(result).toBe("^test$");
    });

    it("escapes special regex characters", () => {
      const result = regexificationRay("/api/users");
      // Forward slash is not a special regex char, so not escaped
      expect(result).toBe("^/api/users$");
    });

    it("escapes brackets and question marks", () => {
      const result = regexificationRay("test[0]?");
      expect(result).toBe("^test\\[0\\]\\?$");
    });

    it("escapes dots", () => {
      const result = regexificationRay("test.example");
      expect(result).toBe("^test\\.example$");
    });
  });
});
