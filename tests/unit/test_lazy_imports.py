"""Tests for the package's lazily imported public names."""

from subprocess import run
from sys import executable
from unittest import TestCase

import typedframes


def _fresh(code: str) -> str:
    """Run `code` in a new interpreter, which has imported nothing yet, and return what it printed."""
    result = run([executable, "-I", "-c", code], capture_output=True, text=True, check=True)
    return result.stdout.strip()


class TestLazyImports(TestCase):
    """Importing the package must stay cheap; the public names load when first used."""

    def test_should_not_load_the_schema_classes_when_the_package_is_imported(self) -> None:
        """Test that `import typedframes` leaves the submodules for later."""
        # arrange
        code = "import sys, typedframes; print('typedframes.base_schema' in sys.modules)"

        # act
        loaded = _fresh(code)

        # assert
        self.assertEqual("False", loaded)

    def test_should_load_a_public_name_from_its_submodule_on_first_use(self) -> None:
        """Test that asking for a name imports it, and later lookups skip the hook."""
        # arrange
        code = (
            "import sys, typedframes; "
            "name = 'BaseSchema'; "
            "first = getattr(typedframes, name); "
            "print('typedframes.base_schema' in sys.modules, name in vars(typedframes), "
            "first is getattr(typedframes, name))"
        )

        # act
        result = _fresh(code)

        # assert
        self.assertEqual("True True True", result)

    def test_should_resolve_every_public_name(self) -> None:
        """Test that each name in `__all__` can be imported from the package."""
        # act and assert
        for name in typedframes.__all__:
            with self.subTest(name=name):
                self.assertTrue(callable(getattr(typedframes, name)))

    def test_should_raise_attribute_error_for_a_name_the_package_does_not_have(self) -> None:
        """Test that the lazy hook does not make every name resolvable."""
        # arrange
        name = "no_such_name"

        # act and assert
        with self.assertRaisesRegex(AttributeError, "has no attribute 'no_such_name'"):
            getattr(typedframes, name)
