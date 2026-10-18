import pandas as pd


def make(rows, names) -> pd.DataFrame:
    frame = pd.DataFrame(columns=names, data=rows)
    for name in frame.columns:
        pass
    return frame
