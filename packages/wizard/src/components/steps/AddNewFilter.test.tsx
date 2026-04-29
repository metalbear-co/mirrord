import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "../../test/test-utils";
import AddNewFilter from "./AddNewFilter";
import { ConfigDataContext } from "../UserDataContext";

describe("AddNewFilter", () => {
  const mockSetConfig = vi.fn();
  const mockConfig = {
    feature: {
      network: {
        incoming: { mode: "steal" as const },
      },
    },
  };

  const renderWithContext = (config = mockConfig) => {
    return render(
      <ConfigDataContext.Provider value={{ config, setConfig: mockSetConfig }}>
        <AddNewFilter type="header" placeholder="eg. x-test: value" />
      </ConfigDataContext.Provider>,
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
  });

  it("renders the filter input", () => {
    renderWithContext();

    expect(
      screen.getByPlaceholderText("eg. x-test: value"),
    ).toBeInTheDocument();
  });

  it("renders the Add button", () => {
    renderWithContext();

    expect(screen.getByRole("button", { name: /add/i })).toBeInTheDocument();
  });

  it("renders the match type selector with Regex Match as default", () => {
    renderWithContext();

    // The combobox should show "Regex Match" as the selected value
    const combobox = screen.getByRole("combobox");
    expect(combobox).toHaveTextContent("Regex Match");
  });

  it("allows changing match type to Exact Match", () => {
    renderWithContext();

    // Click on the select trigger
    const combobox = screen.getByRole("combobox");
    fireEvent.click(combobox);

    // Select Exact Match from the dropdown
    const exactMatchOptions = screen.getAllByText("Exact Match");
    // Click the one in the dropdown (last one)
    fireEvent.click(exactMatchOptions[exactMatchOptions.length - 1]);

    expect(combobox).toHaveTextContent("Exact Match");
  });

  it("adds a filter when form is submitted", () => {
    renderWithContext();

    const input = screen.getByPlaceholderText("eg. x-test: value");
    fireEvent.change(input, { target: { value: "x-test: true" } });

    const form = input.closest("form");
    fireEvent.submit(form!);

    expect(mockSetConfig).toHaveBeenCalled();
  });

  it("clears input after adding filter", () => {
    renderWithContext();

    const input = screen.getByPlaceholderText(
      "eg. x-test: value",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "x-test: true" } });

    const form = input.closest("form");
    fireEvent.submit(form!);

    expect(input.value).toBe("");
  });

  it("does not add filter when input is empty", () => {
    renderWithContext();

    const input = screen.getByPlaceholderText("eg. x-test: value");
    const form = input.closest("form");
    fireEvent.submit(form!);

    expect(mockSetConfig).not.toHaveBeenCalled();
  });

  it("renders with path type placeholder", () => {
    render(
      <ConfigDataContext.Provider
        value={{ config: mockConfig, setConfig: mockSetConfig }}
      >
        <AddNewFilter type="path" placeholder="eg. /api/v1/test" />
      </ConfigDataContext.Provider>,
    );

    expect(screen.getByPlaceholderText("eg. /api/v1/test")).toBeInTheDocument();
  });
});
