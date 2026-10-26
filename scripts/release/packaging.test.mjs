import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "selai-packaging-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const scripts = {};
  for (const name of ["collect-bundles", "assemble-release"]) {
    const script = join(root, `${name}.sh`);
    // Windows checkouts may use CRLF; runners execute these scripts with LF.
    writeFileSync(script, readFileSync(new URL(`./${name}.sh`, import.meta.url), "utf8").replaceAll("\r\n", "\n"));
    scripts[name] = script;
  }
  function file(path, content) {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, content);
  }
  const artifacts = join(root, "artifacts");
  function platform(name, files) {
    const dir = join(artifacts, `installers-${name}`);
    const sums = [];
    for (const [name, content] of Object.entries(files)) {
      file(join(dir, name), content);
      sums.push(`${createHash("sha256").update(content).digest("hex")}  ${name}\n`);
    }
    file(join(dir, "SHA256SUMS"), sums.join(""));
    return dir;
  }
  function run(name, args, env = {}) {
    return spawnSync("bash", [scripts[name], ...args], {
      encoding: "utf8",
      env: { ...process.env, ...env },
    });
  }
  return { root, artifacts, file, platform, run };
}

const unixOnly = { skip: process.platform === "win32" };

test("both macOS architectures stage and assemble with distinct app archives", unixOnly, (t) => {
  const f = fixture(t);
  const bin = join(f.root, "bin");
  // Linux has no ditto. Stand in for its copy to exercise archive naming and
  // release assembly; native archive fidelity still needs macOS validation.
  f.file(join(bin, "ditto"), `#!/usr/bin/env node
const fs = require('node:fs');
const [app, archive] = process.argv.slice(-2);
fs.copyFileSync(app + '/Contents/MacOS/selai-desktop', archive);
`);
  chmodSync(join(bin, "ditto"), 0o755);
  for (const [platform, arch] of [["macos", "aarch64"], ["macos-intel", "x64"]]) {
    const bundle = join(f.root, `bundle-${arch}`);
    f.file(join(bundle, "dmg", `Selai_0.4.0_${arch}.dmg`), `${arch} disk image`);
    f.file(join(bundle, "macos", "Selai.app", "Contents", "MacOS", "selai-desktop"), `${arch} executable`);
    const result = f.run("collect-bundles", [bundle, join(f.artifacts, `installers-${platform}`), arch], {
      PATH: `${bin}:${process.env.PATH}`,
    });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  }
  f.platform("linux", { "selai.deb": "linux installer" });
  f.platform("windows", { "Selai.exe": "windows installer" });
  f.file(join(f.artifacts, "sbom", "selai.cdx.json"), "{}");
  const dist = join(f.root, "dist");
  const result = f.run("assemble-release", [f.artifacts, dist]);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  for (const arch of ["aarch64", "x64"]) {
    assert.equal(readFileSync(join(dist, `Selai_${arch}.app.zip`), "utf8"), `${arch} executable`);
    assert.equal(readFileSync(join(dist, `Selai_0.4.0_${arch}.dmg`), "utf8"), `${arch} disk image`);
  }
  const checksum = spawnSync("sha256sum", ["-c", "SHA256SUMS"], { cwd: dist, encoding: "utf8" });
  assert.equal(checksum.status, 0, checksum.stdout + checksum.stderr);
  assert.ok(readFileSync(join(dist, "SHA256SUMS"), "utf8").includes("sbom-cyclonedx.tar.gz"));
});

test("release assembly rejects a missing Intel artifact", unixOnly, (t) => {
  const f = fixture(t);
  for (const platform of ["linux", "windows", "macos"]) {
    f.platform(platform, { [`${platform}.installer`]: platform });
  }
  const result = f.run("assemble-release", [f.artifacts, join(f.root, "dist")]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /missing .*installers-macos-intel\/SHA256SUMS/);
});

test("release assembly checks Intel checksums before publishing its files", unixOnly, (t) => {
  const f = fixture(t);
  for (const platform of ["linux", "windows", "macos", "macos-intel"]) {
    f.platform(platform, { [`${platform}.installer`]: platform });
  }
  f.file(join(f.artifacts, "installers-macos-intel", "macos-intel.installer"), "corrupted");
  const result = f.run("assemble-release", [f.artifacts, join(f.root, "dist")]);
  assert.notEqual(result.status, 0);
  assert.match(result.stdout, /macos-intel\.installer: FAILED/);
});
