# Verified real, currently-published tag as of 2026-09-13:
# https://hub.docker.com/r/rapidsai/base/tags -> 26.08-cuda12-py3.11
FROM rapidsai/base:26.08-cuda12-py3.11

WORKDIR /workspace

# typedframes' runtime API (BaseSchema/Column/Annotated) has no cuDF-specific code --
# only the Rust *checker* does -- so the published wheel is sufficient here; we are not
# running `typedframes check` inside this container, only the real cuDF pipeline.
RUN pip install --no-cache-dir typedframes

COPY run_example.py .

CMD ["python", "run_example.py"]
