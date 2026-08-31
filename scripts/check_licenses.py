"""Verify the licences of every dependency installed in the development environment.

This backs the ``verify-licences`` invoke task. It uses scancode-toolkit, which
performs *text-based* licence detection: it matches the actual licence text a
distribution ships against a database of ~2500 known licence texts, the same way
GitHub's own licence detector works. That is a deliberate step up from reading a
package's declared PyPI metadata, which is self-reported and frequently absent,
stale, or too vague to act on.

Detection strategy, per installed distribution:

1. Match the licence text of every licence file bundled in the distribution's
   ``.dist-info`` directory (``LICENSE``, ``COPYING``, ``NOTICE``, ...). This is
   the authoritative source and is what scancode is genuinely good at.
2. Only when a distribution bundles no licence file at all, fall back to its
   declared metadata -- the ``License-Expression``/``License`` field and any
   ``License ::`` trove classifiers -- resolved through scancode's SPDX symbol
   table and, failing that, scancode's text matcher.

Policy: a distribution passes when at least one detected licence falls into an
allowed category. This mirrors what licensecheck gated on for this project (an
MIT-licensed project: permissive, public domain and weak/limited copyleft are
fine; strong copyleft, proprietary and unidentifiable ones are not).

The "at least one allowed licence" rule rather than "no disallowed licence" is
deliberate. Many distributions concatenate the licence texts of their vendored
third-party code into their own ``LICENSE`` file -- pandas ships BSD-3-Clause
alongside the notices of everything it vendors, which includes GPL text. Failing
on any disallowed *match* would reject pandas, which is plainly wrong: the grant
pandas makes to us is BSD-3-Clause. Requiring a positive allowed match still
fails a dependency that offers only a disallowed licence, or none at all.
"""

from __future__ import annotations

import fnmatch
from dataclasses import dataclass
from importlib.metadata import Distribution, distributions
from pathlib import Path

# Distributions exempted from the check. Carried over from the ``ignore_packages``
# list of the licensecheck configuration this check replaces.
IGNORED_PACKAGES = frozenset(
    {
        "mypy-extensions",
        "typedframes",
        "pyrefly",
        # Declares a bare "BSD" in its metadata -- not an SPDX identifier, with no
        # trove classifier and no bundled licence file -- so nothing in the
        # installed artifact pins down which BSD variant it is. Upstream is
        # BSD-3-Clause.
        "ply",
    }
)

# scancode assigns every licence it knows to a category. These are the categories a
# dependency of an MIT-licensed project may carry.
ALLOWED_CATEGORIES = frozenset({"Permissive", "Public Domain", "Copyleft Limited"})

# scancode's marker for "there is clearly a licence reference here but it cannot be
# identified". It matches on almost every package, so it is never proof of a licence.
UNSTATED_CATEGORY = "Unstated License"

LICENCE_FILE_PATTERNS = ("LICEN[CS]E*", "COPYING*", "NOTICE*")

# SPDX expression operators, stripped before looking tokens up as licence identifiers.
SPDX_OPERATORS = frozenset({"and", "or", "with"})


@dataclass(frozen=True)
class PackageLicences:
    """The licences detected for one installed distribution.

    Attributes:
        name: Canonical (PEP 503 normalised) distribution name.
        keys: scancode licence keys detected for the distribution.
        categories: scancode categories of ``keys``.
        source: Where the detection came from, for the failure report.
    """

    name: str
    keys: frozenset[str]
    categories: frozenset[str]
    source: str

    @property
    def is_allowed(self) -> bool:
        """Whether at least one detected licence is in an allowed category."""
        return bool(self.categories & ALLOWED_CATEGORIES)


def canonical_name(raw: str) -> str:
    """Normalise a distribution name per PEP 503.

    Args:
        raw: Distribution name as written in metadata or configuration.

    Returns:
        The lower-cased name with runs of ``-``, ``_`` and ``.`` collapsed to ``-``.
    """
    return raw.strip().lower().replace("_", "-").replace(".", "-")


def _dist_info_dir(dist: Distribution) -> Path | None:
    """Locate the ``.dist-info`` directory of an installed distribution.

    Args:
        dist: The installed distribution.

    Returns:
        The distribution's ``.dist-info`` directory, or ``None`` when it records no
        file list to derive it from.
    """
    for file in dist.files or []:
        parts = file.parts
        if parts and parts[0].endswith(".dist-info"):
            return Path(str(file.locate())).parents[len(parts) - 2]
    return None


def _licence_files(dist_info: Path | None) -> list[Path]:
    """Return the licence files bundled in a ``.dist-info`` directory.

    Args:
        dist_info: The distribution's ``.dist-info`` directory, if it has one.

    Returns:
        Every bundled file whose name looks like a licence file, sorted by path.
    """
    if dist_info is None or not dist_info.is_dir():
        return []
    return [
        path
        for path in sorted(dist_info.rglob("*"))
        if path.is_file() and any(fnmatch.fnmatch(path.name.upper(), pattern) for pattern in LICENCE_FILE_PATTERNS)
    ]


def _expression_tokens(expression: str) -> set[str]:
    """Split a licence expression into candidate licence identifiers.

    Args:
        expression: A licence expression such as ``mit OR apache-2.0``.

    Returns:
        The identifier tokens, with SPDX operators and brackets removed.
    """
    cleaned = expression.replace("(", " ").replace(")", " ")
    return {token for token in cleaned.split() if token.lower() not in SPDX_OPERATORS}


def _keys_from_licence_files(files: list[Path]) -> set[str]:
    """Detect licence keys from bundled licence text.

    Args:
        files: Licence files to run scancode's text detection over.

    Returns:
        The scancode licence keys matched anywhere in those files.
    """
    from scancode.api import get_licenses

    keys: set[str] = set()
    for path in files:
        for detection in get_licenses(str(path)).get("license_detections", []):
            for match in detection.get("matches", []):
                keys |= _expression_tokens(match["license_expression"])
    return keys


def _declared_statements(dist: Distribution) -> list[str]:
    """Collect the licence statements a distribution declares in its metadata.

    Args:
        dist: The installed distribution.

    Returns:
        The declared licence expression or field, plus any licence classifiers.
    """
    metadata = dist.metadata
    declared = [value for field in ("License-Expression", "License") for value in metadata.get_all(field) or []]
    classifiers = [
        classifier for classifier in metadata.get_all("Classifier") or [] if classifier.startswith("License ::")
    ]
    return [statement for statement in [*declared, *classifiers] if statement]


def _keys_from_declared(statements: list[str]) -> set[str]:
    """Resolve declared licence statements to scancode licence keys.

    Tries scancode's SPDX symbol table first, so a bare ``MIT`` or ``BSD-3-Clause``
    resolves exactly, then falls back to scancode's text matcher for prose such as
    a trove classifier.

    Args:
        statements: Declared licence statements from package metadata.

    Returns:
        The scancode licence keys the statements resolve to.
    """
    from licensedcode.cache import get_index, get_spdx_symbols

    spdx_symbols = get_spdx_symbols()
    keys: set[str] = set()
    for statement in statements:
        for token in _expression_tokens(statement):
            symbol = spdx_symbols.get(token.lower())
            if symbol is not None:
                keys.add(symbol.wrapped.key)
    if keys:
        return keys

    index = get_index()
    for statement in statements:
        for match in index.match(query_string=statement):
            keys |= _expression_tokens(match.rule.license_expression)
    return keys


def _categories(keys: set[str]) -> set[str]:
    """Map scancode licence keys to their categories.

    Args:
        keys: scancode licence keys.

    Returns:
        The categories of every key scancode recognises, excluding the
        "unidentifiable licence reference" marker.
    """
    from licensedcode.cache import get_licenses_db

    database = get_licenses_db()
    categories = {database[key].category for key in keys if key in database}
    return categories - {UNSTATED_CATEGORY}


def _inspect(dist: Distribution, name: str) -> PackageLicences:
    """Detect the licences of one installed distribution.

    Args:
        dist: The installed distribution.
        name: Its canonical name.

    Returns:
        What was detected, and where it was detected from.
    """
    files = _licence_files(_dist_info_dir(dist))
    if files:
        keys = _keys_from_licence_files(files)
        source = "licence files: " + ", ".join(sorted({path.name for path in files}))
    else:
        keys = _keys_from_declared(_declared_statements(dist))
        source = "declared metadata (no licence file bundled)"
    return PackageLicences(
        name=name,
        keys=frozenset(keys),
        categories=frozenset(_categories(keys)),
        source=source,
    )


def check_licences() -> tuple[list[PackageLicences], int]:
    """Check every distribution installed in the current environment.

    Returns:
        The distributions that failed the policy, and how many were checked.
    """
    # distributions() yields one entry per sys.path location a distribution is
    # importable from, so the same package can come back more than once.
    results: dict[str, PackageLicences] = {}
    for dist in distributions():
        name = canonical_name(dist.metadata["Name"] or "")
        if not name or name in IGNORED_PACKAGES or name in results:
            continue
        results[name] = _inspect(dist, name)
    failures = sorted(
        (result for result in results.values() if not result.is_allowed),
        key=lambda result: result.name,
    )
    return failures, len(results)


def format_failure(failure: PackageLicences) -> str:
    """Render one policy failure as a human-readable line.

    Args:
        failure: The failing distribution.

    Returns:
        A description of what was detected and why it was rejected.
    """
    if not failure.keys:
        detail = "no licence detected"
    else:
        detail = (
            f"detected {', '.join(sorted(failure.keys))} "
            f"(categories: {', '.join(sorted(failure.categories)) or 'none'})"
        )
    return f"  {failure.name}: {detail} -- from {failure.source}"
