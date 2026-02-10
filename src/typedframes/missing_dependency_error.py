"""Error for missing optional dependencies."""


class MissingDependencyError(ImportError):
    """Raised when an optional dependency is not installed."""

    def __init__(self, package: str, feature: str) -> None:
        """Initialize with the missing package and feature that requires it.

        Args:
            package: The name of the missing package (e.g., "polars").
            feature: The feature that requires this package (e.g., "Column.col").

        """
        self.package = package
        self.feature = feature
        super().__init__(f"{package} is required for {feature}. Install it with: pip install typedframes[{package}]")
