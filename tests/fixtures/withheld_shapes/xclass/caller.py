from .service import InferenceService


def from_local() -> None:
    sut = InferenceService()
    frame = sut.predict([1])
    frame["score"].sum()


def from_parameter(sut: InferenceService) -> None:
    frame = sut.predict([1])
    frame["score"].sum()


class Job(InferenceService):
    def run(self) -> None:
        frame = self.predict([1])
        frame["score"].sum()
