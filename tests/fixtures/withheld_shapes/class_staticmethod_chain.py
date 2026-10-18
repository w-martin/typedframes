import pandas as pd


class Pipeline:
    @staticmethod
    def derive(frame, name, exclude=None) -> pd.DataFrame:
        return frame

    def run(self, agg, other) -> None:
        derived = self.derive(other, "c6", exclude=[self.key])
        merged = (
            agg.merge(derived, on=self.key, how="outer").merge(other, on=self.key, how="outer").sort_values(self.key)
        )
