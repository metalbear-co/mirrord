import "./globals.css";
import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Capabilities Rust Demo",
  description: "Pane-based frontend for the capabilities-rust backend"
};

export default function RootLayout({
  children
}: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
