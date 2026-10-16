// SSH project URIs must be spelled exactly as src-tauri/src/agent/host.rs
// spells them, and listed by the saved host's name, never its raw id.
import { expect, it } from "vitest";
import { isSsh, parseSsh, projectHost, projectLabel, sshUri } from "./sshPath";

const HOSTS = [{ id: "a1b2c3d4e5", name: "Dev box" }, { id: "ffff000011", name: "CI runner" }];

it("builds and parses URIs like the backend", () => {
  expect(sshUri("a1b2c3d4e5", "/home/me/proj")).toBe("ssh://a1b2c3d4e5/home/me/proj");
  expect(sshUri("a1b2c3d4e5", "/home/me/proj/")).toBe("ssh://a1b2c3d4e5/home/me/proj");
  expect(sshUri("h", "/")).toBe("ssh://h/");
  expect(sshUri("me@box:22", "/p")).toBe("ssh://me%40box%3A22/p");
  expect(parseSsh("ssh://me%40box%3A22/p")).toEqual({ host: "me@box:22", path: "/p" });
  expect(parseSsh("SSH://h/x/./y//z/")).toEqual({ host: "h", path: "/x/y/z" });
  expect(parseSsh("ssh://h")).toEqual({ host: "h", path: "/" });
  expect(parseSsh("ssh://h/with space/ünï")?.path).toBe("/with space/ünï");
  for (const bad of ["ssh://me@box/p", "ssh:///p", "ssh://%zz/p", "ssh://h/p\nx", "C:\\x", "/home/me"]) expect(parseSsh(bad)).toBeNull();
  expect(isSsh("Ssh://x/") && !isSsh("/ssh://x")).toBe(true);
});

it("labels SSH projects as folder · host name", () => {
  expect(projectLabel("ssh://a1b2c3d4e5/home/me/proj", HOSTS)).toBe("proj · Dev box");
  expect(projectLabel("ssh://ffff000011/", HOSTS)).toBe("/ · CI runner");
  expect(projectLabel("ssh://gone000000/srv/app", HOSTS)).toBe("app · unknown host");
  expect(projectLabel("ssh://a1b2c3d4e5/srv/app", null)).toBe("app · unknown host");
  expect(projectHost("ssh://a1b2c3d4e5/x", HOSTS)).toBe("Dev box");
  expect(projectHost("D:\\work\\app", HOSTS)).toBeNull();
});

it("leaves local and WSL paths to baseName", () => {
  expect(projectLabel("D:\\work\\app", HOSTS)).toBe("app");
  expect(projectLabel("/home/me/proj/", HOSTS)).toBe("proj");
  expect(projectLabel("\\\\wsl.localhost\\Ubuntu\\home\\me\\app", HOSTS)).toBe("app");
});
