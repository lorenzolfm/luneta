{ pkgs }:
let
  nodeTypes = pkgs.writeText "node.d.ts" ''
    declare module "node:crypto" {
      export interface Hash {
        update(data: string): Hash;
        digest(encoding: "hex"): string;
      }
      export function createHash(algorithm: "sha256"): Hash;
    }

    declare module "node:fs" {
      export interface Stats {
        readonly uid: number;
        readonly mode: number;
        readonly size: number;
        isDirectory(): boolean;
        isFile(): boolean;
      }
      export function appendFileSync(path: string, data: string): void;
      export function lstatSync(path: string): Stats;
      export function mkdirSync(
        path: string,
        options?: { readonly mode?: number; readonly recursive?: boolean },
      ): string | undefined;
      export function readFileSync(path: string, encoding: "utf8"): string;
      export function renameSync(oldPath: string, newPath: string): void;
      export function rmSync(path: string, options?: { readonly force?: boolean }): void;
      export function writeFileSync(path: string, data: string): void;
    }

    declare module "node:path" {
      export function join(...paths: string[]): string;
    }

    declare namespace NodeJS {
      interface Process {
        readonly argv: readonly string[];
        readonly env: Record<string, string | undefined>;
        readonly pid: number;
        readonly getuid?: () => number;
      }
    }

    declare const process: NodeJS.Process;

    declare const Buffer: {
      from(input: string, encoding: "utf8"): { toString(encoding: "base64"): string };
      byteLength(input: string, encoding: "utf8"): number;
    };
  '';
in
pkgs.writeShellApplication {
  name = "lune-typescript";
  runtimeInputs = [ pkgs.typescript ];
  text = ''
    exec tsc \
      --noEmit \
      --strict \
      --exactOptionalPropertyTypes \
      --noUncheckedIndexedAccess \
      --erasableSyntaxOnly \
      --target es2024 \
      --module nodenext \
      --moduleResolution nodenext \
      ${nodeTypes} \
      "$@"
  '';
}
