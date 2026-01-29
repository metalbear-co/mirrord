import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "../../test/test-utils";
import BoilerplateStep from "./BoilerplateStep";
import { ConfigDataContext } from "../UserDataContext";
import { DefaultConfig } from "../UserDataContext";

describe("BoilerplateStep", () => {
  const mockSetConfig = vi.fn();

  const renderWithContext = (config = DefaultConfig) => {
    return render(
      <ConfigDataContext.Provider value={{ config, setConfig: mockSetConfig }}>
        <BoilerplateStep />
      </ConfigDataContext.Provider>,
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
  });

  it("renders all three mode options", () => {
    renderWithContext();

    expect(screen.getByText("Mirror Mode")).toBeInTheDocument();
    expect(screen.getByText("Filtering Mode")).toBeInTheDocument();
    expect(screen.getByText("Replace Mode")).toBeInTheDocument();
  });

  it('shows "Recommended" badge on Mirror Mode', () => {
    renderWithContext();

    expect(screen.getByText("Recommended")).toBeInTheDocument();
  });

  it("displays descriptive text for each mode", () => {
    renderWithContext();

    expect(
      screen.getByText(/Copy incoming traffic to your local environment/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Selectively intercept traffic based on HTTP headers/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Completely replace the remote service/),
    ).toBeInTheDocument();
  });

  it("displays feature badges for each mode", () => {
    renderWithContext();

    // Mirror mode features
    expect(screen.getByText("Non-disruptive")).toBeInTheDocument();
    expect(screen.getByText("Safe for production")).toBeInTheDocument();

    // Filtering mode features
    expect(screen.getByText("Selective traffic")).toBeInTheDocument();
    expect(screen.getByText("Header-based routing")).toBeInTheDocument();

    // Replace mode features
    expect(screen.getByText("Full replacement")).toBeInTheDocument();
    expect(screen.getByText("Scale down remote")).toBeInTheDocument();
  });

  it("calls setConfig when Mirror Mode is selected", () => {
    renderWithContext();

    const mirrorButton = screen.getByText("Mirror Mode").closest("button");
    fireEvent.click(mirrorButton!);

    expect(mockSetConfig).toHaveBeenCalled();
    const newConfig = mockSetConfig.mock.calls[0][0];
    expect(newConfig.feature?.network?.incoming?.mode).toBe("mirror");
  });

  it("calls setConfig when Filtering Mode is selected", () => {
    renderWithContext();

    const filterButton = screen.getByText("Filtering Mode").closest("button");
    fireEvent.click(filterButton!);

    expect(mockSetConfig).toHaveBeenCalled();
    const newConfig = mockSetConfig.mock.calls[0][0];
    expect(newConfig.feature?.network?.incoming?.mode).toBe("steal");
    expect(newConfig.feature?.copy_target?.enabled).toBe(false);
  });

  it("calls setConfig when Replace Mode is selected", () => {
    renderWithContext();

    const replaceButton = screen.getByText("Replace Mode").closest("button");
    fireEvent.click(replaceButton!);

    expect(mockSetConfig).toHaveBeenCalled();
    const newConfig = mockSetConfig.mock.calls[0][0];
    expect(newConfig.feature?.network?.incoming?.mode).toBe("steal");
    expect(newConfig.feature?.copy_target?.enabled).toBe(true);
    expect(newConfig.feature?.copy_target?.scale_down).toBe(true);
  });

  it("shows selected state for mirror mode when config has mirror mode", () => {
    const mirrorConfig = {
      feature: {
        network: {
          incoming: { mode: "mirror" as const },
        },
      },
    };

    renderWithContext(mirrorConfig);

    const mirrorButton = screen.getByText("Mirror Mode").closest("button");
    expect(mirrorButton).toHaveClass("border-primary");
  });

  it("renders the heading text", () => {
    renderWithContext();

    expect(
      screen.getByText("How do you want to interact with remote traffic?"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Choose a mode that fits your development workflow"),
    ).toBeInTheDocument();
  });
});
