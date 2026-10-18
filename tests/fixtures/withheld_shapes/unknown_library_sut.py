import pandas as pd


class InferenceService:
    def predict(self, rows) -> pd.DataFrame:
        return pd.DataFrame(rows)

    def predict_untyped(self, rows):
        return pd.DataFrame(rows)


def annotated_local() -> None:
    sut = InferenceService()
    frame = sut.predict([1])
    frame["score"].sum()


def unannotated_local() -> None:
    sut = InferenceService()
    frame = sut.predict_untyped([1])
    frame["score"].sum()


def from_parameter(sut: InferenceService) -> None:
    frame = sut.predict([1])
    frame["score"].sum()


def annotated_variable(sut) -> None:
    frame: pd.DataFrame = sut.run([1])
    frame["score"].sum()


def derived_from_frame(sut) -> None:
    raw = pd.read_csv("a.csv")
    frame = sut.transform(raw)
    frame["score"].sum()


class Pipeline:
    def __init__(self, sut: InferenceService) -> None:
        self.sut = sut

    def run(self) -> None:
        frame = self.sut.predict([1])
        frame["score"].sum()
        other = self.sut.predict_untyped([1])
        other["score"].sum()
