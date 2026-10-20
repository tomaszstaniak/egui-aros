# Workflow scripts

Implement stack-specific setup/build/test entry points here when the tools are selected; no commands or environment loader are supplied by this structural template.

For a shell workflow, `env.sh` is a versioned mechanism that discovers the project and local configuration, validates settings, and exports the required values. `../local.env.example` documents its interface; the user copies it to ignored `../local.env` for actual machine paths. Do not alter the example to store local values.

Preserve explicit environment overrides: environment > local file > portable defaults. A naive unconditional sourcing of assignments can overwrite overrides; the implemented loader must preserve the documented precedence. Resolve relative values against the directory containing the configuration file. Check invocation from a different cwd and paths containing spaces. Name the supported shells and provide stack equivalents for other hosts where needed. Git ignore status is not a deletion policy.

Document actual commands and configuration keys in [development](../docs/development.md).
