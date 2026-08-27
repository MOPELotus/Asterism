"""Compatibility import for the Fluent AI settings page.

The desktop shell uses :mod:`ai_settings_v2` as the single implementation.
Keeping this import path avoids breaking integrations that imported the old
page while preventing a second, divergent settings UI from being maintained.
"""

from .ai_settings_v2 import AISettingsPage

__all__ = ["AISettingsPage"]
