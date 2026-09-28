import sys

with open('CLAUDE.md') as f:
    lines = f.readlines()
head_line = lines[232]  # line 233, 0-indexed
main_line = lines[234]  # line 235
print("HEAD len:", len(head_line))
print("MAIN len:", len(main_line))
# find common prefix
i = 0
while i < min(len(head_line), len(main_line)) and head_line[i] == main_line[i]:
    i += 1
print("common prefix len:", i)
print("prefix end context:", repr(head_line[max(0, i - 100):i + 100]))

# find common suffix
j = 0
while j < min(len(head_line), len(main_line)) - i and head_line[-1 - j] == main_line[-1 - j]:
    j += 1
print("common suffix len:", j)
print("suffix start context:", repr(head_line[max(0, len(head_line) - j - 100):len(head_line) - j + 100]))

print("---HEAD MIDDLE---")
print(head_line[i:len(head_line) - j])
print("---MAIN MIDDLE---")
print(main_line[i:len(main_line) - j])
