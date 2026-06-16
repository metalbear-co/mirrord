import type { ReactNode } from "react";
import "./globals.css";
import type { Metadata } from "next";

export const metadata: Metadata = {
  title: "Capabilities Rust Demo",
  description:
    "Sticky endpoint switcher frontend for the capabilities-rust backend",
};

export default function RootLayout({
  children,
}: Readonly<{ children: ReactNode }>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
