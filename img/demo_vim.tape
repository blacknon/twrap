Output img/demo_vim.gif

Require ./target/debug/twrap

Set Shell "zsh"
Set FontSize 20
Set Width 1480
Set Height 720
Set Framerate 30
Set Padding 24
Set CursorBlink true
Set TypingSpeed 32ms

# Open vim
Type "./target/debug/twrap -e '(?i)error' -R 'user=[^ ]+' 'user=hidden' -- vim ./tmp/demo.txt"
Enter

Sleep 2s
Type "i"
Type "status=ok"
Sleep 1s
Enter
Type "user=super_user"
Sleep 3s
Enter
Type "ERROR: token expired"
Sleep 2s
Escape
Sleep 1s
Type ":wq!"
Enter
Sleep 1s

# Check file
Type "cat ./tmp/demo.txt"
Enter
Sleep 3s

# Open with bat
Type "./target/debug/twrap -e '(?i)error' -R 'user=[^ ]+' 'user=hidden' -- less -R ./tmp/demo.txt"
Enter
Sleep 3s
