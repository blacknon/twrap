Output img/demo_htop_screenshot.gif

Require ./target/debug/twrap

Set Shell "zsh"
Set FontSize 20
Set Width 1480
Set Height 720
Set Framerate 30
Set Padding 24
Set CursorBlink true
Set TypingSpeed 32ms

Type "./target/debug/twrap -p 'htop-demo-{capture_kind}-' -b s=screenshot -M blacknon -- htop"
Enter
Sleep 3s
Type "s"
Sleep 2s
Type "q"
Sleep 1s

Type "ls -1 ./tmp/twrap-artifacts/htop-demo-*"
Enter
Sleep 3s
