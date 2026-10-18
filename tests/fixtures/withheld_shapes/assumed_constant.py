import pandas as pd


class Runner:
    RESULT_COLUMN = "result"
    MODEL_COLUMN = "model"

    def run(self) -> None:
        frame = pd.read_csv("a.csv")
        frame[self.RESULT_COLUMN] = 1
        frame[self.MODEL_COLUMN] = 2
