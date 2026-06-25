# DAUx Automation

Sample-accurate parameter automation is modeled separately from plain parameter
values so hosts can record and replay curves without blocking the audio thread.

## Types

- `DAUx/Automation/AutomationPoint.h` — `{ sample_offset, value }`
- `DAUx/Automation/ParameterChangeQueue.h` — per-parameter point lists for one block
- `DAUx/Component/ProcessData.h` — `input_parameter_changes` / `output_parameter_changes`

## Realtime rules

- The audio thread **reads** `input_parameter_changes` during `DAUxProcessor.process`.
- Plugins may **write** output changes (e.g. linked parameters) to `output_parameter_changes`.
- Allocation and host notification happen on the **controller** thread via
  `DAUxHostServices.begin_edit` / `perform_edit` / `end_edit`.

Legacy `daux_plugin_vtable` parameter callbacks remain unchanged for existing plugins.
